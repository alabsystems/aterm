// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::Value;

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
        Self::validate_fields(application, role, version, None, false)?;
        Ok(Self {
            scope: Scope::fresh(trust)?,
            application: application.into(),
            role: role.into(),
            version,
            name: None,
            deterministic: false,
        })
    }
    /// Independent offline opens of an application role name the same table.
    pub fn application(trust: Trust, application: &str, role: &str, version: u32) -> Result<Self> {
        Self::validate_fields(application, role, version, None, true)?;
        Ok(Self {
            scope: application_scope(trust, application, role)?,
            application: application.into(),
            role: role.into(),
            version,
            name: None,
            deterministic: true,
        })
    }
    pub(crate) fn is_twin_of(&self, retired: &Self) -> bool {
        retired.version.checked_add(1) == Some(self.version)
            && self.scope == retired.scope
            && self.application == retired.application
            && self.role == retired.role
            && self.name == retired.name
            && self.deterministic == retired.deterministic
    }
    fn validate_fields(
        application: &str,
        role: &str,
        version: u32,
        name: Option<&str>,
        deterministic: bool,
    ) -> Result<()> {
        if !crate::crypto::name(application)
            || !crate::crypto::name(role)
            || application == "aterm.catalog"
            || version == 0
            || (deterministic && name.is_some())
            || name.is_some_and(|name| {
                name.is_empty() || name.len() > 128 || name.chars().any(char::is_control)
            })
        {
            return Err(Error::new("invalid table declaration"));
        }
        Ok(())
    }
    fn validate(&self) -> Result<()> {
        Self::validate_fields(
            &self.application,
            &self.role,
            self.version,
            self.name.as_deref(),
            self.deterministic,
        )?;
        if self.deterministic
            && self.scope != application_scope(self.scope.trust(), &self.application, &self.role)?
        {
            return Err(Error::new("invalid table declaration"));
        }
        Ok(())
    }
    fn encode_unchecked(&self) -> Result<Vec<u8>> {
        #[derive(serde::Serialize)]
        struct WireDescriptor<'a> {
            admission: &'static str,
            application: &'a str,
            deterministic: bool,
            generation: u64,
            kind: &'static str,
            merge: &'static str,
            name: Option<&'a str>,
            role: &'a str,
            table: &'a str,
            version: u32,
        }
        let table = aterm_codec::hex::encode_lower(self.scope.table());
        aterm_json::to_vec(&WireDescriptor {
            admission: "commons-writers-v1",
            application: &self.application,
            deterministic: self.deterministic,
            generation: self.scope.generation(),
            kind: "create-table-v1",
            merge: "causal-log-v1",
            name: self.name.as_deref(),
            role: &self.role,
            table: &table,
            version: self.version,
        })
        .map_err(|_| Error::new("table declaration could not be encoded"))
    }
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        self.encode_unchecked()
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
        if descriptor.encode_unchecked()? != payload {
            return Err(Error::new("table declaration is not canonical"));
        }
        Ok(descriptor)
    }
}

pub(crate) fn application_scope(trust: Trust, application: &str, role: &str) -> Result<Scope> {
    let mut sha = aterm_digest::Sha256::new();
    sha.update(b"aterm/commons/application-table/1\0");
    sha.update(trust.network);
    sha.update(trust.authority);
    sha.update(trust.genesis);
    for word in [application, role] {
        sha.update((word.len() as u64).to_le_bytes());
        sha.update(word.as_bytes());
    }
    sha.update(1u64.to_le_bytes());
    let hash = sha.finalize();
    let mut table: [u8; 16] = hash[..16].try_into().expect("fixed digest width");
    table[0] |= 0x80;
    Scope::new(trust, table, 1)
}

pub(super) fn catalog_scope(trust: Trust) -> Scope {
    application_scope(trust, "aterm.catalog", "tables").expect("nonzero catalog scope")
}
