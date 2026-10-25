// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::{Value, json};

/// Immutable, inert table metadata. Schema names never load code or authorize
/// credentials. A schema version change keeps its identity and conflicts until
/// an explicit migration is supported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub scope: Scope,
    pub application: String,
    pub role: String,
    pub version: u32,
    pub name: Option<String>,
    pub deterministic: bool,
}
impl Descriptor {
    /// An independently created table, even if another table has the same name.
    pub fn fresh(trust: Trust, application: &str, role: &str, version: u32) -> Result<Self> {
        let descriptor = Self {
            scope: Scope::fresh(trust)?,
            application: application.into(),
            role: role.into(),
            version,
            name: None,
            deterministic: false,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }
    /// Independent offline opens of an application role name the same table.
    pub fn application(trust: Trust, application: &str, role: &str, version: u32) -> Result<Self> {
        let descriptor = Self {
            scope: application_scope(trust, application, role)?,
            application: application.into(),
            role: role.into(),
            version,
            name: None,
            deterministic: true,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }
    fn validate(&self) -> Result<()> {
        if !crate::crypto::name(&self.application)
            || !crate::crypto::name(&self.role)
            || self.application == "aterm.catalog"
            || self.version == 0
            || (self.deterministic && self.name.is_some())
            || self.name.as_ref().is_some_and(|name| {
                name.is_empty() || name.len() > 128 || name.chars().any(char::is_control)
            })
            || (self.deterministic
                && self.scope
                    != application_scope(self.scope.trust(), &self.application, &self.role)?)
        {
            return Err(Error::new("invalid table declaration"));
        }
        Ok(())
    }
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        aterm_json::to_vec(&json!({
            "kind": "create-table-v1", "table": aterm_codec::hex::encode_lower(self.scope.table()),
            "generation": self.scope.generation(), "application": self.application,
            "role": self.role, "version": self.version, "name": self.name,
            "deterministic": self.deterministic,
            "merge": "causal-log-v1", "admission": "commons-writers-v1"
        }))
        .map_err(|_| Error::new("table declaration could not be encoded"))
    }
    pub(crate) fn decode(trust: Trust, payload: &[u8]) -> Result<Self> {
        if payload.len() > 4096 {
            return Err(Error::new("table declaration exceeds its bound"));
        }
        let value: Value = aterm_json::from_slice(payload)
            .map_err(|_| Error::new("invalid table declaration encoding"))?;
        let object = value
            .as_object()
            .ok_or_else(|| Error::new("invalid table declaration"))?;
        if object.len() != 10
            || value.get("kind").and_then(Value::as_str) != Some("create-table-v1")
            || value.get("merge").and_then(Value::as_str) != Some("causal-log-v1")
            || value.get("admission").and_then(Value::as_str) != Some("commons-writers-v1")
        {
            return Err(Error::new("unsupported table declaration"));
        }
        let word = |key| {
            value
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| Error::new("missing table declaration field"))
        };
        let name = match value.get("name") {
            Some(Value::Null) => None,
            Some(Value::String(name)) => Some(name.clone()),
            _ => return Err(Error::new("invalid table display name")),
        };
        let descriptor = Self {
            scope: Scope::new(
                trust,
                crate::database::decode_hex(word("table")?)?,
                value
                    .get("generation")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| Error::new("invalid table generation"))?,
            )?,
            application: word("application")?.into(),
            role: word("role")?.into(),
            version: value
                .get("version")
                .and_then(Value::as_u64)
                .and_then(|n| n.try_into().ok())
                .ok_or_else(|| Error::new("invalid table schema version"))?,
            name,
            deterministic: value
                .get("deterministic")
                .and_then(Value::as_bool)
                .ok_or_else(|| Error::new("invalid table identity mode"))?,
        };
        descriptor.validate()?;
        // One wire representation prevents duplicate keys and alternative JSON
        // spellings from becoming distinct statements of identical metadata.
        if descriptor.encode()? != payload {
            return Err(Error::new("table declaration is not canonical"));
        }
        Ok(descriptor)
    }
}

fn application_scope(trust: Trust, application: &str, role: &str) -> Result<Scope> {
    let mut bytes = b"aterm/commons/application-table/1\0".to_vec();
    bytes.extend_from_slice(&trust.network);
    bytes.extend_from_slice(&trust.authority);
    bytes.extend_from_slice(&trust.genesis);
    for word in [application, role] {
        bytes.extend_from_slice(&(word.len() as u64).to_le_bytes());
        bytes.extend_from_slice(word.as_bytes());
    }
    bytes.extend_from_slice(&1u64.to_le_bytes());
    let hash = aterm_digest::Sha256::digest(&bytes);
    let mut table: [u8; 16] = hash[..16].try_into().expect("fixed digest width");
    table[0] |= 0x80;
    Scope::new(trust, table, 1)
}

pub(super) fn catalog_scope(trust: Trust) -> Scope {
    application_scope(trust, "aterm.catalog", "tables").expect("nonzero catalog scope")
}
