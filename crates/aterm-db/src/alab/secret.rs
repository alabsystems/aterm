// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The plaintext exit for Secret cells. Each successful reveal first durably records
//! only its coordinates, surface and time in a local journal, never in Commons.

use crate::{
    Error, Result,
    typed::secret::{self, Cell, Sealer},
};
use std::path::{Path, PathBuf};

/// The caller's consuming surface, not arbitrary text that could contain a value.
#[derive(Clone, Copy, Debug)]
pub enum Surface {
    Terminal,
    Grid,
    SignIn,
    Phone,
}

impl Surface {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal",
            Self::Grid => "Grid",
            Self::SignIn => "SignIn",
            Self::Phone => "Phone",
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

    #[cfg(unix)]
    fn record(&self, cell: Cell<'_>, surface: Surface, when_ms: u64) -> Result<()> {
        use std::io::Write;
        let directory = self
            .path
            .parent()
            .ok_or_else(|| Error::new("invalid reveal journal"))?;
        aterm_journal::create_private_dir_durable(directory)
            .map_err(|_| Error::new("cannot create local reveal journal"))?;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
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
        if !meta.is_file()
            || meta.uid() != aterm_uds::peer::our_uid()
            || meta.nlink() != 1
            || meta.mode() & 0o077 != 0
        {
            return Err(Error::new(
                "local reveal journal is not a private regular file",
            ));
        }
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

/// Open a cell with its epoch's audience key, journal once, then pass plaintext to its
/// consuming surface. No plaintext leaves on authentication or journal failure.
///
/// # Errors
/// The seal does not authenticate, its coordinates are invalid, or the local journal fails.
pub fn reveal<T>(
    journal: &Journal,
    token: &str,
    audience_key: &[u8; 32],
    cell: Cell<'_>,
    surface: Surface,
    when_ms: u64,
    consume: impl FnOnce(&str) -> T,
) -> Result<T> {
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
    let secret = secret::open(token, audience_key, cell)?;
    journal.record(cell, surface, when_ms)?;
    Ok(consume(secret.plaintext()))
}

/// [`reveal`] through a table session's [`Sealer`]: `token`, stored at `cell` of the sealer's
/// table, opens under its epoch's key, which stays inside the session, is journalled once, then
/// reaches `consume`. What a registry reader that holds no key uses (`alab::mail::reveal`).
///
/// # Errors
/// As [`reveal`], `cell` names another table, or the token's epoch key is unavailable here.
pub fn reveal_sealed<T>(
    journal: &Journal,
    sealer: &dyn Sealer,
    token: &str,
    cell: Cell<'_>,
    surface: Surface,
    when_ms: u64,
    consume: impl FnOnce(&str) -> T,
) -> Result<T> {
    if cell.table != sealer.table()
        || !crate::typed::pk_grammar(cell.row)
        || !crate::typed::column_name(cell.column)
        || !crate::typed::column_name(cell.collection)
    {
        return Err(Error::new("invalid secret cell coordinates"));
    }
    let secret = sealer.open(cell.collection, cell.row, cell.column, token)?;
    journal.record(cell, surface, when_ms)?;
    Ok(consume(secret.plaintext()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn reveal_records_metadata_once_and_fails_closed() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let journal = Journal::at(dir.path());
        let cell = Cell {
            table: "alab/registry",
            collection: "mailbox",
            row: "mb-fixture",
            column: "password",
        };
        let value = "fixture-password-not-for-logs";
        let token = secret::Secret::new(value.into())
            .seal(&[3; 32], &[4; 32], cell)
            .unwrap();
        assert!(
            reveal(
                &journal,
                &token,
                &[3; 32],
                cell,
                Surface::SignIn,
                123,
                |plain| plain == value
            )
            .unwrap()
        );
        let log = std::fs::read_to_string(&journal.path).unwrap();
        assert_eq!(log.lines().count(), 1);
        assert!(!log.contains(value));
        assert!(!log.contains(&token));
        assert!(log.contains("SignIn"));
        assert!(
            reveal(
                &journal,
                &token,
                &[8; 32],
                cell,
                Surface::Grid,
                124,
                |_| panic!("must not reveal")
            )
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(&journal.path).unwrap(), log);
        let blocked = Journal {
            path: dir.path().to_owned(),
        };
        assert!(
            reveal(
                &blocked,
                &token,
                &[3; 32],
                cell,
                Surface::Grid,
                124,
                |_| panic!("must not reveal")
            )
            .is_err()
        );
    }
}
