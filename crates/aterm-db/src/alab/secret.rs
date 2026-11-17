// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The plaintext exit for a table's secret cells: a value is read where its generation holds it
//! (the sealed token in its cell, or the secret-store entry a `SecretRef` cell names,
//! [`crate::typed::secret::Store`]) and handed to its one consumer. Each
//! successful reveal first durably records only its coordinates, surface and time in a local
//! journal, never in Commons.

use crate::{
    Error, Result,
    typed::secret::{Cell, Store},
};
use std::path::{Path, PathBuf};

/// The caller's consuming surface, not arbitrary text that could contain a value.
#[derive(Clone, Copy, Debug)]
pub enum Surface {
    /// A harness's sign-in: a mailbox's password or TOTP seed (`alab::mail`).
    SignIn,
}

impl Surface {
    #[cfg(unix)]
    const fn as_str(self) -> &'static str {
        match self {
            Self::SignIn => "SignIn",
        }
    }
}

/// A local-only reveal journal. Explicit paths allow isolated tests and lab instances.
pub struct Journal {
    path: PathBuf,
}

impl Journal {
    /// Use a private directory reserved for reveal records, separate from the shared spool.
    pub fn at(directory: &Path) -> Self {
        Self {
            path: directory.join("reveals.jsonl"),
        }
    }

    /// Resolve the user's local journal, respecting an isolated instance's state directory.
    ///
    /// # Errors
    /// The state directory is unavailable.
    pub fn local() -> Result<Self> {
        aterm_types::dirs::state_dir()
            .map(|p| Self::at(&p.join("secret-reveals")))
            .ok_or_else(|| Error::new("local reveal journal is unavailable"))
    }

    /// Whether a reveal could be recorded here, as far as that shows with nothing written:
    /// the journal's file, where there is one, is the private regular file [`Self::record`]
    /// requires before it appends. A file or folder not made yet is ready, since `record`
    /// makes both; an append or a sync that then fails (a full volume) shows only at the
    /// reveal. A look that decides whether a sign-in can start asks this
    /// ([`super::mail::usable`]'s callers), so it does not say yes where every reveal fails.
    ///
    /// # Errors
    /// The file is not a private regular file of this user, or cannot be inspected.
    #[cfg(unix)]
    pub fn ready(&self) -> Result<()> {
        match std::fs::symlink_metadata(&self.path) {
            Ok(meta) => private_regular(&meta),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(Error::new("cannot inspect reveal journal")),
        }
    }

    /// [`Self::ready`] where no journal can be written.
    ///
    /// # Errors
    /// Always: this platform has no local reveal journal.
    #[cfg(not(unix))]
    pub fn ready(&self) -> Result<()> {
        let _ = &self.path;
        Err(Error::new(
            "local reveal journal is unavailable on this platform",
        ))
    }

    #[cfg(unix)]
    fn record(&self, cell: Cell<'_>, surface: Surface, when_ms: u64) -> Result<()> {
        use std::io::Write;
        let directory = self
            .path
            .parent()
            .ok_or_else(|| Error::new("invalid reveal journal"))?;
        aterm_journal::create_private_dir_durable(directory)
            .map_err(|_| Error::new("cannot create local reveal journal"))?;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
            .open(&self.path)
            .map_err(|_| Error::new("cannot open local reveal journal"))?;
        let meta = file
            .metadata()
            .map_err(|_| Error::new("cannot inspect reveal journal"))?;
        private_regular(&meta)?;
        #[derive(serde::Serialize)]
        struct RevealRecord<'a> {
            collection: &'a str,
            column: &'a str,
            row: &'a str,
            surface: &'static str,
            table: &'a str,
            when_ms: u64,
        }
        let mut line = aterm_json::to_vec(&RevealRecord {
            collection: cell.collection,
            column: cell.column,
            row: cell.row,
            surface: surface.as_str(),
            table: cell.table,
            when_ms,
        })
        .map_err(|_| Error::new("cannot encode local reveal record"))?;
        line.push(b'\n');
        file.write_all(&line)
            .and_then(|()| aterm_durable::durable(&file))
            .and_then(|()| aterm_durable::durable_dir(directory))
            .map_err(|_| Error::new("cannot commit local reveal record"))
    }

    #[cfg(not(unix))]
    fn record(&self, _: Cell<'_>, _: Surface, _: u64) -> Result<()> {
        let _ = &self.path;
        Err(Error::new(
            "local reveal journal is unavailable on this platform",
        ))
    }
}

/// The journal file as [`Journal::record`] appends to it: a regular file of this user alone,
/// with one name.
#[cfg(unix)]
fn private_regular(meta: &std::fs::Metadata) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    if !meta.is_file()
        || meta.uid() != aterm_uds::peer::our_uid()
        || meta.nlink() != 1
        || meta.mode() & 0o077 != 0
    {
        return Err(Error::new(
            "local reveal journal is not a private regular file",
        ));
    }
    Ok(())
}

pub(crate) fn record_reveal(
    journal: &Journal,
    cell: Cell<'_>,
    surface: Surface,
    when_ms: u64,
) -> Result<()> {
    if !crate::typed::pk_grammar(cell.row)
        || !crate::typed::column_name(cell.column)
        || !crate::typed::column_name(cell.collection)
        || cell
            .table
            .split_once('/')
            .is_none_or(|(app, role)| !crate::crypto::name(app) || !crate::crypto::name(role))
    {
        return Err(Error::new("invalid secret cell coordinates"));
    }
    journal.record(cell, surface, when_ms)
}

/// Read the store entry `reference` (a `SecretRef` cell's value at `cell`) names, journal once,
/// then pass the value to its consuming surface. `None`, journalling nothing, where the store
/// holds no such entry. No plaintext leaves on a store or journal failure.
///
/// # Errors
/// The coordinates are invalid, `reference` names no entry, another vault's or one that is not
/// `cell`'s own ([`crate::typed::secret::bound_entry`]), the store cannot be read, or the local
/// journal fails.
pub fn reveal<T>(
    journal: &Journal,
    store: &dyn Store,
    reference: &aterm_json::Value,
    cell: Cell<'_>,
    surface: Surface,
    when_ms: u64,
    consume: impl FnOnce(&str) -> T,
) -> Result<Option<T>> {
    let entry = crate::typed::secret::bound_entry(reference, cell, &store.vault()?)?;
    let Some(secret) = store.get(entry)? else {
        return Ok(None);
    };
    record_reveal(journal, cell, surface, when_ms)?;
    Ok(Some(consume(secret.plaintext())))
}

/// Every store entry a candidate of a secret cell (a secret-named `SecretRef` column) names in
/// `view` of `table`: the entries a [`crate::typed::secret::sweep`] keeps.
pub fn named_entries(
    table: &crate::typed::Table,
    view: &crate::typed::View,
) -> std::collections::BTreeSet<String> {
    let mut named = std::collections::BTreeSet::new();
    for collection in table.collections {
        let columns: Vec<&str> = collection
            .columns
            .iter()
            .filter(|c| {
                matches!(c.ty, crate::typed::Ty::SecretRef) && crate::typed::secret_named(c.name)
            })
            .map(|c| c.name)
            .collect();
        if columns.is_empty() {
            continue;
        }
        for row in view.project(collection).rows.values() {
            for column in &columns {
                let Some(field) = row.fields.get(*column) else {
                    continue;
                };
                named.extend(
                    field
                        .candidates()
                        .iter()
                        .filter_map(|c| crate::typed::secret::entry_of(&c.value.x))
                        .map(str::to_owned),
                );
            }
        }
    }
    named
}

/// The registry's secret store on this Mac: its `aterm secrets` vault
/// ([`super::vault::Vault`]). It reads a value in place, and takes one only once every Mac
/// reading the vault writes no vault entry to a file (the fleet floor,
/// `aterm_secrets::client::fleet_in_place`), which no Mac passes while no such release is named.
#[cfg(unix)]
#[must_use]
pub fn store(paths: &aterm_secrets::client::Paths) -> super::vault::Vault {
    super::vault::Vault::at(paths.clone())
}

/// A [`Store`] that holds nothing and takes nothing, for tests of a write whose store refuses: a
/// read or a write of a value fails with one sentence, and nothing is owed.
#[cfg(all(test, unix))]
#[derive(Clone, Copy, Debug, Default)]
pub struct Closed;

#[cfg(all(test, unix))]
impl Closed {
    fn closed<T>() -> Result<T> {
        Err(Error::new("a closed store takes no registry secret"))
    }
}

#[cfg(all(test, unix))]
impl Store for Closed {
    fn vault(&self) -> Result<String> {
        Self::closed()
    }

    fn admit(&self) -> Result<()> {
        Self::closed()
    }

    fn put(&self, _entry: &str, _value: &crate::typed::secret::Secret) -> Result<()> {
        Self::closed()
    }

    fn get(&self, _entry: &str) -> Result<Option<crate::typed::secret::Secret>> {
        Self::closed()
    }

    fn remove(&self, _entry: &str) -> Result<()> {
        Self::closed()
    }

    fn owe(&self, _entries: &[String]) -> Result<()> {
        Self::closed()
    }

    fn orphaned(&self) -> Result<std::collections::BTreeSet<String>> {
        Ok(std::collections::BTreeSet::new())
    }

    fn paid(&self, _entries: &std::collections::BTreeSet<String>) -> Result<()> {
        Ok(())
    }

    fn cleared(&self, _entries: &std::collections::BTreeSet<String>) -> Result<()> {
        Ok(())
    }
}

/// What a secret write of the registry through `handle` owes the store once it has been tried:
/// where it landed (`landed`: the entries it put and those it replaced), it pays its puts and
/// removes exactly the replaced entries ([`crate::typed::secret::retire`]); then what ended runs
/// left owed is swept ([`crate::typed::secret::sweep`]). Both are asked of
/// [`crate::att::Handle::sole_generation`] when they act, after the write: while this Mac holds another
/// generation of the registry (a roll in flight, or a sealed one not yet released), the entries
/// the registry names are not all this view's, so nothing is removed and the entries stay owed.
///
/// # Errors
/// The owed record or the registry cannot be read or written.
#[cfg(unix)]
pub fn settle_write(
    handle: &crate::att::Handle,
    store: &dyn crate::typed::secret::Store,
    landed: Option<(
        &std::collections::BTreeSet<String>,
        &std::collections::BTreeSet<String>,
    )>,
) -> crate::Result<crate::typed::secret::Settled> {
    use crate::typed::secret;
    let mut settled = secret::Settled::default();
    if let Some((put, replaced)) = landed {
        store.paid(put)?;
        if !replaced.is_empty() && handle.sole_generation(&crate::att::REGISTRY) {
            settled = settled + secret::retire(store, replaced)?;
        }
    }
    Ok(settled + housekeep(handle, store)?)
}

/// Collect what ended runs left owed in the registry's store
/// ([`crate::typed::secret::sweep`]): each entry no cell of the registry `handle` reaches names
/// is removed, asked of [`crate::att::Handle::sole_generation`] as [`settle_write`] asks it.
/// Every secret write and import into a generation whose cells name store entries ends with it,
/// so an entry a failed removal or a refused write left owed is collected by the next one.
///
/// # Errors
/// The owed record or the registry cannot be read or written.
#[cfg(unix)]
pub fn housekeep(
    handle: &crate::att::Handle,
    store: &dyn crate::typed::secret::Store,
) -> crate::Result<crate::typed::secret::Settled> {
    crate::typed::secret::sweep(store, || {
        if !handle.sole_generation(&crate::att::REGISTRY) {
            return Ok(None);
        }
        Ok(Some(named_entries(
            handle.compiled().table,
            &handle.view()?,
        )))
    })
}

#[cfg(all(test, unix))]
#[path = "secret_tests.rs"]
mod tests;
