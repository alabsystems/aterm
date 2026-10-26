// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::Value;

/// An owned observation of the bounded, complete proposal closure matched to
/// a root decision. It is not a permission by itself and does not stay current.
/// No vault entry values are present: those remain in `Preparation::credential`.
pub struct Configuration {
    scope: Scope,
    key: String,
    digest: Digest,
    values: BTreeMap<(String, String), Value>,
}

impl Configuration {
    pub(super) fn capture(paths: &Paths, record: &Record) -> Result<Self> {
        let Action::Prepare(p) = &record.request().action else {
            return Err(Error::new("account is stopped"));
        };
        let snapshot = super::super::proposals::Proposals::at(paths.clone()).snapshot()?;
        let plan = super::super::plans::inspect(&snapshot, &p.key)?;
        let digest = aterm_codec::hex::encode_lower(&p.configuration);
        let row = snapshot
            .cells
            .get(&("accounts".into(), p.key.clone()))
            .and_then(|c| c.candidates.first())
            .ok_or_else(|| Error::new("account proposal is missing"))?;
        if plan["complete"].as_bool() != Some(true)
            || snapshot.scope.digest() != p.table
            || plan["configuration_digest"].as_str() != Some(digest.as_str())
            || row.value["provider"].as_str() != Some(p.provider.as_str())
            || row.value["method"].as_str() != Some(p.method.as_str())
        {
            return Err(Error::new(
                "account proposal does not match its preparation decision",
            ));
        }
        let mut values = BTreeMap::new();
        for dependency in plan["dependencies"]
            .as_array()
            .ok_or_else(|| Error::new("account plan has no closure"))?
        {
            let collection = dependency["collection"]
                .as_str()
                .ok_or_else(|| Error::new("account plan has an invalid collection"))?;
            let key = dependency["key"]
                .as_str()
                .ok_or_else(|| Error::new("account plan has an invalid key"))?;
            let key = (collection.to_owned(), key.to_owned());
            let cell = snapshot
                .cells
                .get(&key)
                .ok_or_else(|| Error::new("account closure is incomplete"))?;
            if cell.stopped || cell.candidates.len() != 1 {
                return Err(Error::new("account closure is not unique"));
            }
            values.insert(key, cell.candidates[0].value.clone());
        }
        if !values.contains_key(&("accounts".into(), p.key.clone())) {
            return Err(Error::new("account plan omitted its selected account"));
        }
        Ok(Self {
            scope: snapshot.scope,
            key: p.key.clone(),
            digest: p.configuration,
            values,
        })
    }

    /// Scope of the proposals whose configuration was checked.
    pub const fn scope(&self) -> Scope {
        self.scope
    }

    /// Configuration digest signed by the authority, independent of labels and
    /// same-value rewrites of the selected account.
    pub const fn digest(&self) -> Digest {
        self.digest
    }

    /// Selected account's immutable key in this observation.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The account value whose configuration was checked.
    pub fn account(&self) -> &Value {
        &self.values[&("accounts".into(), self.key.clone())]
    }

    /// Look up only a member of this checked closure. Unrelated records are
    /// deliberately absent, even when the source table also contained them.
    pub fn get(&self, collection: &str, key: &str) -> Option<&Value> {
        self.values.get(&(collection.to_owned(), key.to_owned()))
    }
}
