// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stable machine identities and reliable human names. Names are assignments, not
//! identities; all prior signed assignments remain aliases in the history view.
use crate::{Error, Result, crypto::name};
use aterm_json::Value;

/// Canonical machine name, including legacy high numbers such as m100.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Name(pub u32);
impl Name {
    /// Parse m1, M2, m100, etc.; zero and ambiguous leading zeroes are refused.
    pub fn parse(text: &str) -> Result<Self> {
        let digits = text
            .strip_prefix(['m', 'M'])
            .ok_or_else(|| Error::new("machine name must be m followed by a positive number"))?;
        if digits.starts_with('0')
            || digits.is_empty()
            || !digits.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(Error::new(
                "machine number must be positive without leading zeroes",
            ));
        }
        let number = digits
            .parse()
            .map_err(|_| Error::new("machine number exceeds the supported range"))?;
        Ok(Self(number))
    }
    /// Lowercase stable display form.
    pub fn canonical(&self) -> String {
        format!("m{}", self.0)
    }
}
/// Display slot m3.2; resolved through the registry, never through hostname guessing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    /// Name when this label was assigned.
    pub machine: Name,
    /// One-based account number.
    pub number: u32,
}
impl Slot {
    /// Dot notation or historical dash notation. A bare m3 is its first slot.
    pub fn parse(text: &str) -> Result<Self> {
        let (machine, number) = text
            .split_once(['.', '-'])
            .map_or((text, "1"), |(m, n)| (m, n));
        let machine = Name::parse(machine)?;
        if number.starts_with('0')
            || number.is_empty()
            || !number.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(Error::new("account slot must be a positive number"));
        }
        let number = number
            .parse()
            .map_err(|_| Error::new("account slot exceeds the supported range"))?;
        Ok(Self { machine, number })
    }
    /// Canonical account slot.
    pub fn canonical(&self) -> String {
        format!("{}.{}", self.machine.canonical(), self.number)
    }
}
/// Validate a stable-id keyed assignment; ingestion separately enforces authority.
pub fn validate(key: &str, value: &Value) -> Result<()> {
    crate::database::decode_hex::<16>(key)?;
    let object = value
        .as_object()
        .ok_or_else(|| Error::new("machine assignment must be an object"))?;
    if object
        .keys()
        .any(|key| !["name", "legacy_names", "hostnames", "details"].contains(&key.as_str()))
    {
        return Err(Error::new("unknown machine assignment field"));
    }
    Name::parse(
        value
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new("machine needs a canonical name"))?,
    )?;
    for field in ["legacy_names", "hostnames"] {
        if let Some(list) = value.get(field) {
            let list = list
                .as_array()
                .filter(|l| l.len() <= 64)
                .ok_or_else(|| Error::new("machine names must be a bounded list"))?;
            if !list.iter().all(|v| v.as_str().is_some_and(name)) {
                return Err(Error::new("invalid machine alias or hostname"));
            }
        }
    }
    if let Some(details) = value.get("details") {
        crate::contacts::validate_details(details)?;
    }
    Ok(())
}
/// All names this signed assignment contributes to the monotone history index.
pub fn names(value: &Value) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(text) = value.get("name").and_then(Value::as_str)
        && let Ok(n) = Name::parse(text)
    {
        names.push(n.canonical());
    }
    if let Some(legacy) = value.get("legacy_names").and_then(Value::as_array) {
        for n in legacy.iter().filter_map(Value::as_str) {
            names.push(Name::parse(n).map_or_else(|_| n.to_owned(), |n| n.canonical()));
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_names_and_account_slots_keep_their_meaning() {
        assert_eq!(Name::parse("M100").unwrap().canonical(), "m100");
        assert_eq!(Slot::parse("m3.2").unwrap().number, 2);
        assert_eq!(Slot::parse("m27-2").unwrap().canonical(), "m27.2");
        assert_eq!(Slot::parse("m3").unwrap().number, 1);
        for bad in ["m0", "m01", "m-1", "m3.2"] {
            assert!(Name::parse(bad).is_err());
        }
    }
}
