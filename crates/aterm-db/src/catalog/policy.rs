// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Local subscription policy. This never grants access to another network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub enabled: bool,
    pub excluded: BTreeSet<Digest>,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: true,
            excluded: BTreeSet::new(),
        }
    }
}
impl Policy {
    /// Whether local sharing includes `table`: disabling sharing or excluding its catalog
    /// excludes every table, and an individual exclusion excludes only that table.
    #[must_use]
    pub fn includes(&self, catalog: Digest, table: Digest) -> bool {
        self.enabled && !self.excluded.contains(&catalog) && !self.excluded.contains(&table)
    }

    /// The policy `aterm.toml` at `path` gives, read as the window reads its settings: a symlink
    /// (a dotfiles checkout, home-manager) is followed to a file this user or root owns and
    /// others cannot write ([`aterm_secrets::fsx::read_aterm_config`]), as `[swarm] enabled` is
    /// read ([`aterm_secrets::swarm::enabled_at`]).
    ///
    /// # Errors
    /// Unsafe or unreadable configuration files, invalid TOML, or invalid subscription settings.
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = aterm_secrets::fsx::read_aterm_config(path, 1024 * 1024).map_err(|why| {
            Error::new(&format!(
                "Commons configuration is not a file of this user's or root's that others cannot \
                 write: {why}"
            ))
        })?;
        let Some(bytes) = bytes else {
            return Ok(Self::default());
        };
        let text =
            std::str::from_utf8(&bytes).map_err(|_| Error::new("invalid Commons configuration"))?;
        let value: aterm_toml::Value =
            aterm_toml::from_str(text).map_err(|_| Error::new("invalid Commons configuration"))?;
        Self::from_value(&value)
    }

    /// Parse the subscription policy from an already admitted configuration document,
    /// allowing an application to evaluate all its local settings from one file read.
    ///
    /// # Errors
    /// A non-table document or invalid swarm or Commons settings.
    pub fn from_value(value: &aterm_toml::Value) -> Result<Self> {
        if value.as_table().is_none() {
            return Err(Error::new("invalid Commons configuration"));
        }
        let swarm = aterm_secrets::swarm::enabled_in_value(value)
            .map_err(|why| Error::new(&why.to_string()))?;
        let Some(commons) = value.get("commons") else {
            return Ok(Self {
                enabled: swarm,
                excluded: BTreeSet::new(),
            });
        };
        if !matches!(commons, aterm_toml::Value::Table(_)) {
            return Err(Error::new("invalid Commons configuration section"));
        }
        if commons
            .as_table()
            .expect("checked table")
            .keys()
            .any(|key| !matches!(key.as_str(), "enabled" | "exclude_tables"))
        {
            return Err(Error::new("unknown Commons configuration setting"));
        }
        let enabled = match commons.get("enabled") {
            None => true,
            Some(aterm_toml::Value::Boolean(enabled)) => *enabled,
            Some(_) => return Err(Error::new("Commons enabled setting must be boolean")),
        };
        let mut excluded = BTreeSet::new();
        if let Some(value) = commons.get("exclude_tables") {
            let array = value
                .as_array()
                .filter(|array| array.len() <= 1024)
                .ok_or_else(|| Error::new("Commons table exclusions must be a bounded array"))?;
            for value in array {
                let word = value
                    .as_str()
                    .ok_or_else(|| Error::new("invalid excluded table"))?;
                let id = crate::database::decode_hex(word)?;
                if !excluded.insert(id) {
                    return Err(Error::new("noncanonical or repeated excluded table"));
                }
            }
        }
        Ok(Self {
            enabled: swarm && enabled,
            excluded,
        })
    }
}

#[cfg(all(test, unix))]
#[path = "policy_tests.rs"]
mod tests;
