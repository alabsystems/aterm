// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fresh application table discovered and repaired by ordinary running hosts.

use super::{HANG, fail, save};
use crate::{Fail, stop};
use astream_wire::sync::Digest;
use aterm_db::{
    catalog::{Descriptor, Directory, State},
    replica::InventorySummary,
    table::{Session, store::Store},
};
use aterm_json::{Value, json};
use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::{Duration, Instant},
};

pub(crate) type Rows = BTreeMap<Digest, Value>;

pub(crate) struct Table(Descriptor);

impl Table {
    pub(crate) fn application(paths: &Paths, application: &str, role: &str) -> Result<Self, Fail> {
        let bootstrap = Bootstrap::required(paths).map_err(fail)?;
        Descriptor::application(bootstrap.trust(), application, role, 1)
            .map(Self)
            .map_err(fail)
    }

    pub(crate) fn publish(&self, paths: &Paths) -> Result<(), Fail> {
        let bootstrap = Bootstrap::required(paths).map_err(fail)?;
        let session = Session::open(&bootstrap).map_err(fail)?;
        Directory::new(paths, bootstrap.trust())
            .publish(&session, &self.0)
            .map_err(fail)?;
        Ok(())
    }

    pub(super) fn declare(paths: &[Paths; 2], out: &Path) -> Result<Self, Fail> {
        let bootstrap = Bootstrap::required(&paths[0]).map_err(fail)?;
        let descriptor =
            Descriptor::fresh(bootstrap.trust(), "startup-data", "items", 1).map_err(fail)?;
        for paths in paths {
            let directory = Directory::new(paths, bootstrap.trust());
            if directory
                .path(descriptor.scope)
                .map_err(fail)?
                .try_exists()
                .map_err(fail)?
            {
                return Err(Fail::die("fresh application table already exists"));
            }
        }
        let table = Self(descriptor);
        save(
            out,
            "table-before.json",
            &json!({
                "table": table.id(),
                "application": "startup-data",
                "role": "items",
                "source_table_present": false,
                "recipient_table_present": false,
            }),
        )?;
        let session = Session::open(&bootstrap).map_err(fail)?;
        Directory::new(&paths[0], bootstrap.trust())
            .publish(&session, &table.0)
            .map_err(fail)?;
        Ok(table)
    }

    pub(crate) fn id(&self) -> String {
        aterm_codec::hex::encode_lower(&self.0.scope.digest())
    }

    /// Append only to an existing store. The recipient never declares or joins it here.
    pub(crate) fn write(&self, paths: &Paths, key: &str) -> Result<(Digest, Value), Fail> {
        let bootstrap = Bootstrap::required(paths).map_err(fail)?;
        let session = Session::open(&bootstrap).map_err(fail)?;
        let directory = Directory::new(paths, bootstrap.trust());
        let mut store = Store::open(
            &session,
            &directory.path(self.0.scope).map_err(fail)?,
            self.0.scope,
        )
        .map_err(fail)?;
        let value = json!({
            "key": key,
            "value": {"quantity": 7, "note": format!("synthetic-{key}")},
        });
        let payload = aterm_json::to_vec(&value).map_err(fail)?;
        let receipt = store.commit_catalog(&session, &payload).map_err(fail)?;
        Ok((receipt.id, value))
    }

    pub(crate) fn observe(
        &self,
        paths: &Paths,
        config: &Path,
    ) -> Result<(Rows, BTreeSet<Digest>), Fail> {
        let bootstrap = Bootstrap::required(paths).map_err(fail)?;
        let session = Session::observe(&bootstrap).map_err(fail)?;
        let directory = Directory::new(paths, bootstrap.trust());
        let catalog = directory
            .snapshot_existing_with_config(&session, config)
            .map_err(fail)?;
        let entry = catalog
            .entries
            .get(&self.0.scope.digest())
            .ok_or_else(|| Fail::die("application declaration has not arrived"))?;
        if entry.state != State::Subscribed
            || entry
                .declarations
                .iter()
                .any(|declaration| declaration.descriptor != self.0)
        {
            return Err(Fail::die(
                "application declaration is excluded or conflicting",
            ));
        }
        let store = Store::observe(
            &session,
            &directory.path(self.0.scope).map_err(fail)?,
            self.0.scope,
        )
        .map_err(fail)?;
        let known = store.inventory(&session).map_err(fail)?.copied().collect();
        let mut rows = Rows::new();
        for record in store.catalog(&session).map_err(fail)? {
            rows.insert(
                record.header().id,
                aterm_json::from_slice(record.payload()).map_err(fail)?,
            );
        }
        Ok((rows, known))
    }

    pub(super) fn received(
        &self,
        paths: &Paths,
        config: &Path,
        expected: &Rows,
        out: &Path,
        name: &str,
    ) -> Result<(), Fail> {
        let ids: BTreeSet<_> = expected.keys().copied().collect();
        let until = Instant::now() + HANG;
        loop {
            stop::check()?;
            if let Ok((rows, known)) = self.observe(paths, config)
                && rows == *expected
                && known == ids
            {
                let inventory = InventorySummary::of(&known);
                return save(
                    out,
                    name,
                    &json!({
                        "table": self.id(),
                        "home": paths.home.display().to_string(),
                        "subscribed": true,
                        "known": inventory.count,
                        "eligible_decrypted": rows.len(),
                        "inventory_sha256": aterm_codec::hex::encode_lower(&inventory.fingerprint),
                        "operations": rows.iter().map(|(operation, value)| json!({
                            "operation": aterm_codec::hex::encode_lower(operation),
                            "value": value,
                        })).collect::<Vec<_>>(),
                        "reader_declared_or_joined_table": false,
                    }),
                );
            }
            if Instant::now() >= until {
                return Err(Fail::die(format!(
                    "ordinary runtime did not converge application table {} ({name})",
                    self.id()
                )));
            }
            stop::sleep(Duration::from_millis(100))?;
        }
    }
}
