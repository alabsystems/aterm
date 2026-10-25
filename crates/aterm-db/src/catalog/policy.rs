// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Local subscription policy. This never grants access to another network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Policy {
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
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = aterm_secrets::fsx::read_owned(path, 1024 * 1024)
            .map_err(|_| Error::new("Commons configuration is not an owned safe file"))?;
        let Some(bytes) = bytes else {
            return Ok(Self::default());
        };
        let text =
            std::str::from_utf8(&bytes).map_err(|_| Error::new("invalid Commons configuration"))?;
        let value: aterm_toml::Value =
            aterm_toml::from_str(text).map_err(|_| Error::new("invalid Commons configuration"))?;
        let Some(commons) = value.get("commons") else {
            return Ok(Self::default());
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
                if word != aterm_codec::hex::encode_lower(&id) || !excluded.insert(id) {
                    return Err(Error::new("noncanonical or repeated excluded table"));
                }
            }
        }
        Ok(Self { enabled, excluded })
    }
}
