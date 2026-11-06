// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::resources::{Allocation, Proposal};
use aterm_digest::Sha256;
use aterm_secrets::commons::journal::Trust;
use std::collections::BTreeMap;

pub(super) const MAX_BYTES: usize = 256 * 1024;
const MAX_RESOURCES: usize = 64;

/// Untrusted proposed resource terms. The allocation remains a proposal until
/// the enclosing current root account pins this resource's complete fingerprint.
pub struct Resource {
    pub allocation: Proposal,
    pub provider: String,
    pub adapter: String,
}

#[cfg(test)]
mod tests;

impl Resource {
    pub(super) fn validate(&self) -> Result<()> {
        if self.allocation.terms().unit != "requests"
            || !crate::crypto::name(&self.provider)
            || !crate::crypto::name(&self.adapter)
        {
            return Err(Error::new(
                "billing v1 requires request-count units and named adapters",
            ));
        }
        Ok(())
    }

    fn reference(&self) -> Reference {
        let mut hash = Sha256::new();
        hash.update(b"aterm/account-billing-resource/1\0");
        for bytes in [
            self.allocation.bytes(),
            self.provider.as_bytes(),
            self.adapter.as_bytes(),
        ] {
            hash.update((bytes.len() as u64).to_be_bytes());
            hash.update(bytes);
        }
        Reference {
            principal: self.allocation.terms().resource,
            policy: hash.finalize(),
        }
    }
}

/// Bounded, untrusted policy proposals held in `ai-billing.toml`. Parsing and
/// fingerprinting confer no authority. Unrelated resources do not change the
/// selected resource's fingerprint; conflicting duplicate principals refuse.
pub struct Policy {
    resources: BTreeMap<Digest, Resource>,
}

impl Policy {
    pub fn new(resources: Vec<Resource>) -> Result<Self> {
        if resources.len() > MAX_RESOURCES {
            return Err(Error::new("too many billing resources"));
        }
        let mut selected = BTreeMap::new();
        for resource in resources {
            resource.validate()?;
            if selected
                .insert(resource.allocation.terms().resource, resource)
                .is_some()
            {
                return Err(Error::new("duplicate billing principal"));
            }
        }
        let policy = Self {
            resources: selected,
        };
        if policy.text().len() > MAX_BYTES {
            return Err(Error::new("billing policy exceeds its byte limit"));
        }
        Ok(policy)
    }

    pub fn parse(raw: &[u8]) -> Result<Self> {
        if raw.len() > MAX_BYTES {
            return Err(Error::new("billing policy exceeds its byte limit"));
        }
        let raw = std::str::from_utf8(raw).map_err(|_| Error::new("billing policy is not text"))?;
        let table: aterm_toml::Table =
            aterm_toml::from_str(raw).map_err(|_| Error::new("billing policy is malformed"))?;
        if table.get("version").and_then(aterm_toml::Value::as_integer) != Some(1)
            || table
                .keys()
                .any(|key| !matches!(key.as_str(), "version" | "resource"))
        {
            return Err(Error::new("unsupported billing policy schema"));
        }
        let mut resources = Vec::new();
        if let Some(rows) = table.get("resource") {
            let rows = rows
                .as_array()
                .filter(|rows| rows.len() <= MAX_RESOURCES)
                .ok_or_else(|| Error::new("billing resources must be a bounded array"))?;
            for row in rows {
                let row = row
                    .as_table()
                    .filter(|row| row.len() == 5)
                    .ok_or_else(|| Error::new("invalid billing resource fields"))?;
                let text = |key| {
                    row.get(key)
                        .and_then(aterm_toml::Value::as_str)
                        .ok_or_else(|| Error::new("missing billing resource field"))
                };
                let principal = crate::database::decode_hex::<32>(text("principal")?)?;
                let pin = crate::database::decode_hex::<32>(text("pin")?)?;
                let encoded = text("allocation")?;
                if encoded.len() > 8192 || encoded.len() % 2 != 0 {
                    return Err(Error::new("invalid billing allocation encoding"));
                }
                let mut bytes = vec![0; encoded.len() / 2];
                if !aterm_codec::hex::decode_into(encoded.as_bytes(), &mut bytes) {
                    return Err(Error::new("invalid billing allocation encoding"));
                }
                let allocation = Proposal::decode(&bytes)?;
                if allocation.terms().resource != principal || allocation.fingerprint() != pin {
                    return Err(Error::new("billing allocation and reference disagree"));
                }
                resources.push(Resource {
                    allocation,
                    provider: text("provider")?.into(),
                    adapter: text("adapter")?.into(),
                });
            }
        }
        Self::new(resources)
    }

    /// Exact terms for inclusion in an account proposal and root review.
    pub fn reference(&self, principal: Digest) -> Option<Reference> {
        self.resources.get(&principal).map(Resource::reference)
    }

    /// Proposed rows in canonical principal order. Reading these rows authenticates nothing.
    pub fn resources(&self) -> impl Iterator<Item = &Resource> {
        self.resources.values()
    }

    /// Extend a proposal without replacing any existing principal. All document
    /// bounds still apply; publishing and root approval are separate operations.
    pub fn with_resource(self, resource: Resource) -> Result<Self> {
        let mut resources = self.resources.into_values().collect::<Vec<_>>();
        resources.push(resource);
        Self::new(resources)
    }

    /// Canonical editable proposal text. This signs nothing and loads no key.
    pub fn text(&self) -> String {
        use std::fmt::Write as _;
        let mut text = String::with_capacity(16 + self.resources.len() * 512);
        text.push_str("version = 1\n");
        for resource in self.resources.values() {
            text.push_str("\n[[resource]]\n");
            for (name, bytes) in [
                ("principal", resource.allocation.terms().resource.as_slice()),
                ("allocation", resource.allocation.bytes()),
                ("pin", &resource.allocation.fingerprint()),
            ] {
                text.push_str(name);
                text.push_str(" = \"");
                aterm_codec::hex::push_lower(&mut text, bytes);
                text.push_str("\"\n");
            }
            let _ = write!(
                text,
                "provider = {:?}\nadapter = {:?}\n",
                resource.provider, resource.adapter
            );
        }
        text
    }

    pub(super) fn select(
        &self,
        reference: Reference,
        trust: Trust,
        provider: &str,
    ) -> Result<(Allocation, String)> {
        let resource = self
            .resources
            .get(&reference.principal)
            .filter(|resource| resource.reference() == reference && resource.provider == provider)
            .ok_or_else(|| Error::new("billing resource differs from root account pin"))?;
        Ok((
            Allocation::verify(
                resource.allocation.bytes(),
                resource.allocation.fingerprint(),
                trust,
            )?,
            resource.adapter.clone(),
        ))
    }
}
