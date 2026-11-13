// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host baseline TOML document (`Baseline`), table discovery, and `{key}` substitution.

use crate::items;
use serde::Deserialize;

/// A whole baseline.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    /// [`crate::SCHEMA`].
    pub schema: u32,
    /// Accounts user-scope items apply to (defaults to inventory `users` when omitted).
    #[serde(default)]
    pub users: Vec<String>,
    /// `[names]`.
    #[serde(default)]
    pub names: Option<items::names::Table>,
    /// `[sharing]`.
    #[serde(default)]
    pub sharing: Option<items::sharing::Table>,
    /// `[firewall]`.
    #[serde(default)]
    pub firewall: Option<items::firewall::Table>,
    /// `[time]`.
    #[serde(default)]
    pub time: Option<items::time::Table>,
    /// `[power]`.
    #[serde(default)]
    pub power: Option<items::power::Table>,
    /// `[updates]`.
    #[serde(default)]
    pub updates: Option<items::updates::Table>,
    /// `[keyboard]`.
    #[serde(default)]
    pub keyboard: Option<items::keyboard::Table>,
    /// `[consumer]`: the catalog presets.
    #[serde(default)]
    pub consumer: Option<items::catalog::Table>,
    /// `[setup]`.
    #[serde(default)]
    pub setup: Option<items::setup::Table>,
    /// `[[defaults]]`: the raw escape hatch.
    #[serde(default)]
    pub defaults: Vec<items::defaults::Row>,
    /// `[lan]`.
    #[serde(default)]
    pub lan: Option<items::lan::Table>,
    /// `[developer]`.
    #[serde(default)]
    pub developer: Option<items::developer::Table>,
    /// `[homebrew]`.
    #[serde(default)]
    pub homebrew: Option<items::homebrew::Table>,
    /// `[[app]]`.
    #[serde(default, rename = "app")]
    pub apps: Vec<items::apps::Row>,
    /// `[aterm]`.
    #[serde(default)]
    pub aterm: Option<items::aterm::Table>,
    /// `[github]`.
    #[serde(default)]
    pub github: Option<items::github::Table>,
    /// `[person]`.
    #[serde(default)]
    pub person: Option<items::person::Table>,
}

/// Parse a baseline document.
/// # Errors
/// The parser's message, or a schema mismatch.
pub fn parse(text: &str) -> Result<Baseline, String> {
    let baseline: Baseline = aterm_toml::from_str(text).map_err(|e| format!("baseline: {e}"))?;
    if baseline.schema != crate::SCHEMA {
        return Err(format!(
            "baseline: schema {} (this aterm reads schema {})",
            baseline.schema,
            crate::SCHEMA
        ));
    }
    Ok(baseline)
}

/// Baseline table names (`names`, `power`, `app`, …) derived from [`Baseline`] fields.
#[must_use]
pub fn tables() -> Vec<&'static str> {
    use serde::de::{self, Visitor};

    struct Fields(&'static [&'static str]);

    impl<'de> de::Deserializer<'de> for &mut Fields {
        type Error = de::value::Error;

        fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Self::Error> {
            Err(de::Error::custom("only the field names are read"))
        }

        fn deserialize_struct<V: Visitor<'de>>(
            self,
            _: &'static str,
            fields: &'static [&'static str],
            _: V,
        ) -> Result<V::Value, Self::Error> {
            self.0 = fields;
            Err(de::Error::custom("only the field names are read"))
        }

        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes
            byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct map enum
            identifier ignored_any
        }
    }

    let mut fields = Fields(&[]);
    let _ = Baseline::deserialize(&mut fields);
    fields
        .0
        .iter()
        .copied()
        .filter(|f| !matches!(*f, "schema" | "users"))
        .collect()
}

/// Check `--only`'s table names against [`tables`].
/// # Errors
/// One line naming the unknown table and every known one.
pub fn check_only(only: &[String]) -> Result<(), String> {
    let known = tables();
    if only.is_empty() {
        return Err(format!(
            "--only takes table names, comma-separated: {}",
            known.join(", ")
        ));
    }
    match only.iter().find(|t| !known.contains(&t.as_str())) {
        Some(unknown) => Err(format!(
            "--only: no baseline table is named {unknown:?}; the tables are {}",
            known.join(", ")
        )),
        None => Ok(()),
    }
}

/// Return `text` keeping only `schema`, `users`, and the tables named in `only` (unchanged when
/// `only` is empty).
/// # Errors
/// The parser's message.
pub fn keep_only(text: &str, only: &[String]) -> Result<String, String> {
    if only.is_empty() {
        return Ok(text.to_owned());
    }
    let mut doc: aterm_toml::Value =
        aterm_toml::from_str(text).map_err(|e| format!("baseline: {e}"))?;
    let Some(table) = doc.as_table_mut() else {
        return Err("baseline: not a table".into());
    };
    let keys: Vec<String> = table.keys().cloned().collect();
    for key in keys {
        if !matches!(key.as_str(), "schema" | "users") && !only.contains(&key) {
            table.remove(&key);
        }
    }
    aterm_toml::to_string(&doc).map_err(|e| format!("baseline: {e}"))
}

/// Substitute `{key}` placeholders in `text` from `vars`, leaving unknown placeholders intact.
/// # Errors
/// The parser's message.
pub fn fill(text: &str, vars: &[(&str, &str)]) -> Result<String, String> {
    let mut doc: aterm_toml::Value =
        aterm_toml::from_str(text).map_err(|e| format!("baseline: {e}"))?;
    fill_value(&mut doc, vars);
    aterm_toml::to_string(&doc).map_err(|e| format!("baseline: {e}"))
}

fn fill_value(value: &mut aterm_toml::Value, vars: &[(&str, &str)]) {
    match value {
        aterm_toml::Value::String(s) => *s = fill_str(s, vars),
        aterm_toml::Value::Array(items) => items.iter_mut().for_each(|v| fill_value(v, vars)),
        aterm_toml::Value::Table(table) => {
            let keys: Vec<String> = table.keys().cloned().collect();
            for key in keys {
                if let Some(mut v) = table.remove(&key) {
                    fill_value(&mut v, vars);
                    table.insert(key, v);
                }
            }
        }
        _ => {}
    }
}

/// Fill `{key}` placeholders in `s` from `vars`.
#[must_use]
pub fn fill_str(s: &str, vars: &[(&str, &str)]) -> String {
    let mut out = s.to_owned();
    for (key, val) in vars {
        out = out.replace(&format!("{{{key}}}"), val);
    }
    out
}

#[cfg(test)]
#[path = "baseline_tests.rs"]
mod tests;
