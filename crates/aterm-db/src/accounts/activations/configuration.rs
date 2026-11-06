// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::Value;

#[cfg(test)]
thread_local! {pub(super) static CAPTURES: std::cell::Cell<usize> = const {std::cell::Cell::new(0)};}

/// An owned observation of the bounded, complete proposal closure matched to
/// a root decision. It is not a permission by itself and does not stay current.
/// No vault entry values are present: those remain in `Preparation::credential`.
pub struct Configuration {
    scope: Scope,
    key: String,
    digest: Digest,
    values: BTreeMap<String, BTreeMap<String, Value>>,
}

impl Configuration {
    pub(super) fn capture_until(
        paths: &Paths,
        record: &Record,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self> {
        Self::capture_request_until(paths, record.request(), deadline)
    }

    pub(super) fn capture_request_until(
        paths: &Paths,
        request: &Request,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self> {
        let proposals = match deadline {
            Some(deadline) => super::super::proposals::Proposals::at_until(paths.clone(), deadline),
            None => super::super::proposals::Proposals::at(paths.clone()),
        };
        Self::from_snapshot(request, proposals.snapshot()?)
    }

    pub(super) fn capture_in_session(
        session: &Session<'_>,
        record: &Record,
        deadline: std::time::Instant,
    ) -> Result<Self> {
        Self::from_snapshot(
            record.request(),
            super::super::proposals::Proposals::snapshot_in_session(session, deadline)?,
        )
    }

    fn from_snapshot(request: &Request, snapshot: crate::documents::log::Snapshot) -> Result<Self> {
        #[cfg(test)]
        CAPTURES.with(|count| count.set(count.get() + 1));
        let (table, key, configuration, provider, method) = match &request.action {
            Action::Prepare(p) => (p.table, &p.key, p.configuration, &p.provider, &p.method),
            Action::Intent(p) => (p.table, &p.key, p.configuration, &p.provider, &p.method),
            Action::Stop => return Err(Error::new("account is stopped")),
        };

        let plan = super::super::plans::inspect(&snapshot, key)?;
        let row = snapshot
            .cells
            .get(&("accounts".into(), key.clone()))
            .and_then(|c| c.candidates.first())
            .ok_or_else(|| Error::new("account proposal is missing"))?;
        if plan["complete"].as_bool() != Some(true)
            || snapshot.scope.digest() != table
            || !plan["configuration_digest"].as_str().is_some_and(|text| {
                crate::database::decode_hex::<32>(text).is_ok_and(|d| d == configuration)
            })
            || row.value["provider"].as_str() != Some(provider.as_str())
            || row.value["method"].as_str() != Some(method.as_str())
        {
            return Err(Error::new(
                "account proposal does not match its preparation decision",
            ));
        }
        let mut values: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
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
            let lookup = (collection.to_owned(), key.to_owned());
            let cell = snapshot
                .cells
                .get(&lookup)
                .ok_or_else(|| Error::new("account closure is incomplete"))?;
            if cell.stopped || cell.candidates.len() != 1 {
                return Err(Error::new("account closure is not unique"));
            }
            values
                .entry(lookup.0)
                .or_default()
                .insert(lookup.1, cell.candidates[0].value.clone());
        }
        if !values
            .get("accounts")
            .is_some_and(|rows| rows.contains_key(key.as_str()))
        {
            return Err(Error::new("account plan omitted its selected account"));
        }
        Ok(Self {
            scope: snapshot.scope,
            key: key.clone(),
            digest: configuration,
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
        &self.values["accounts"][&self.key]
    }

    /// Look up only a member of this checked closure. Unrelated records are
    /// deliberately absent, even when the source table also contained them.
    pub fn get(&self, collection: &str, key: &str) -> Option<&Value> {
        self.values.get(collection).and_then(|rows| rows.get(key))
    }
}
