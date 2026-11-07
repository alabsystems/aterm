// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::BTreeMap;

pub const ENTRY: &str = aterm_secrets::entry_class::AI_BROKER;
pub(super) const MAX_BYTES: usize = 128 * 1024;
pub(super) const MAX_LEASE: Duration = Duration::from_secs(3600);

#[cfg(test)]
mod tests;

/// Root-authored proposal metadata. Only a current authenticated vault read
/// makes this an execution delegation. The producer must survive token rotation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub id: [u8; 16],
    pub producer: Digest,
    pub actor: Actor,
    pub binding: RequestBinding,
    pub lifetime: Duration,
}

impl Grant {
    fn validate(&self) -> Result<()> {
        if self.id == [0; 16]
            || self.producer == [0; 32]
            || self.actor == [0; 48]
            || self.lifetime.is_zero()
            || self.lifetime > MAX_LEASE
            || !self.lifetime.subsec_nanos().is_multiple_of(1_000_000)
        {
            return Err(Error::new("invalid broker grant identity or lifetime"));
        }
        let _ = crate::resources::BoundRequest::new(self.binding.clone(), [1; 16], 1, &[])?;
        Ok(())
    }
}

/// Bounded, closed proposal schema. Parsing authenticates no grant or producer.
pub struct Policy {
    grants: BTreeMap<[u8; 16], Grant>,
}

impl Policy {
    pub fn new(grants: Vec<Grant>) -> Result<Self> {
        if grants.len() > MAX_SESSIONS {
            return Err(Error::new("too many broker grants"));
        }
        let mut selected = BTreeMap::new();
        let mut producers = std::collections::BTreeSet::new();
        for grant in grants {
            grant.validate()?;
            if !producers.insert(grant.producer) {
                return Err(Error::new("duplicate broker producer"));
            }
            if selected.insert(grant.id, grant).is_some() {
                return Err(Error::new("duplicate broker grant"));
            }
        }
        let policy = Self { grants: selected };
        if policy.text().len() > MAX_BYTES {
            return Err(Error::new("broker policy exceeds its byte limit"));
        }
        Ok(policy)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_BYTES {
            return Err(Error::new("broker policy exceeds its byte limit"));
        }
        let text =
            std::str::from_utf8(bytes).map_err(|_| Error::new("broker policy is not text"))?;
        let table: aterm_toml::Table =
            aterm_toml::from_str(text).map_err(|_| Error::new("malformed broker policy"))?;
        if table.get("version").and_then(aterm_toml::Value::as_integer) != Some(1)
            || table
                .keys()
                .any(|k| !matches!(k.as_str(), "version" | "grant"))
        {
            return Err(Error::new("unsupported broker policy schema"));
        }
        let mut grants = Vec::new();
        if let Some(rows) = table.get("grant") {
            let rows = rows
                .as_array()
                .filter(|rows| rows.len() <= MAX_SESSIONS)
                .ok_or_else(|| Error::new("broker grants must be a bounded array"))?;
            for row in rows {
                let row = row
                    .as_table()
                    .filter(|row| row.len() == 10)
                    .ok_or_else(|| Error::new("invalid broker grant fields"))?;
                let text = |key| {
                    row.get(key)
                        .and_then(aterm_toml::Value::as_str)
                        .ok_or_else(|| Error::new("missing broker grant field"))
                };
                let digest = |key| crate::database::decode_hex::<32>(text(key)?);
                let lifetime = row
                    .get("lifetime_ms")
                    .and_then(aterm_toml::Value::as_integer)
                    .and_then(|n| u64::try_from(n).ok())
                    .ok_or_else(|| Error::new("invalid broker grant lifetime"))?;
                grants.push(Grant {
                    id: crate::database::decode_hex(text("id")?)?,
                    producer: digest("producer")?,
                    actor: crate::database::decode_hex(text("actor")?)?,
                    binding: RequestBinding {
                        principal: digest("principal")?,
                        account: digest("account")?,
                        revision: digest("revision")?,
                        credential: digest("credential")?,
                        policy: digest("policy")?,
                        adapter: text("adapter")?.into(),
                    },
                    lifetime: Duration::from_millis(lifetime),
                });
            }
        }
        Self::new(grants)
    }

    pub fn grants(&self) -> impl Iterator<Item = &Grant> {
        self.grants.values()
    }

    pub(super) fn into_grants(self) -> impl Iterator<Item = Grant> {
        self.grants.into_values()
    }

    pub(super) fn grant(&self, id: [u8; 16]) -> Result<&Grant> {
        self.grants
            .get(&id)
            .ok_or_else(|| Error::new("broker grant is absent"))
    }

    pub(super) fn select(&self, selection: &Selection, actor: Actor) -> Result<&Grant> {
        match selection {
            Selection::Grant(id) => {
                let grant = self.grant(*id)?;
                if grant.actor != actor {
                    return Err(denied());
                }
                Ok(grant)
            }
            Selection::Account { account, adapter } => {
                let mut matches = self.grants().filter(|grant| {
                    grant.actor == actor
                        && grant.binding.account == *account
                        && grant.binding.adapter == *adapter
                });
                let grant = matches.next().ok_or_else(denied)?;
                if matches.next().is_some() {
                    return Err(Error::new(
                        "account has ambiguous broker grants; select an explicit grant",
                    ));
                }
                Ok(grant)
            }
        }
    }

    pub fn text(&self) -> String {
        use std::fmt::Write as _;
        let mut text = String::with_capacity(16 + self.grants.len() * 640);
        text.push_str("version = 1\n");
        for grant in self.grants.values() {
            text.push_str("\n[[grant]]\n");
            for (name, bytes) in [
                ("id", grant.id.as_slice()),
                ("producer", &grant.producer),
                ("actor", &grant.actor),
                ("principal", &grant.binding.principal),
                ("account", &grant.binding.account),
                ("revision", &grant.binding.revision),
                ("credential", &grant.binding.credential),
                ("policy", &grant.binding.policy),
            ] {
                text.push_str(name);
                text.push_str(" = \"");
                aterm_codec::hex::push_lower(&mut text, bytes);
                text.push_str("\"\n");
            }
            let _ = write!(
                text,
                "adapter = {:?}\nlifetime_ms = {}\n",
                grant.binding.adapter,
                grant.lifetime.as_millis()
            );
        }
        text
    }
}
