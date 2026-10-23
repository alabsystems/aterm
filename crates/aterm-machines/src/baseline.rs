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

/// Return `text` keeping only `schema`, `users`, and the tables named in `only`.
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
mod tests {
    use super::*;

    #[test]
    fn an_empty_baseline_parses_and_a_wrong_schema_is_refused() {
        let b = parse("schema = 1\n").unwrap();
        assert!(b.power.is_none());
        assert!(b.apps.is_empty());
        let err = parse("schema = 2\n").unwrap_err();
        assert!(err.contains("schema 2"), "{err}");
    }

    #[test]
    fn an_unknown_table_is_refused_rather_than_ignored() {
        let err = parse("schema = 1\n[powr]\nsleep = 0\n").unwrap_err();
        assert!(err.contains("powr"), "{err}");
    }

    #[test]
    fn the_tables_are_the_baselines_own_fields_as_the_file_spells_them() {
        let known = tables();
        for table in [
            "names", "power", "consumer", "defaults", "app", "aterm", "person",
        ] {
            assert!(known.contains(&table), "{table}: {known:?}");
        }
        assert!(
            !known.contains(&"schema") && !known.contains(&"users"),
            "{known:?}"
        );
        assert!(!known.contains(&"apps"), "the file spells it [[app]]");
        check_only(&["names".into(), "app".into()]).unwrap();
        let err = check_only(&["name".into()]).unwrap_err();
        assert!(
            err.contains("\"name\"") && err.contains("names, sharing"),
            "{err}"
        );
        assert!(check_only(&[]).is_err());
    }

    #[test]
    fn only_the_named_tables_and_the_users_stay() {
        let starter = crate::controller::templates::DEFAULT_BASELINE;
        let kept = parse(&keep_only(starter, &["names".into()]).unwrap()).unwrap();
        assert!(kept.names.is_some());
        assert!(kept.power.is_none() && kept.keyboard.is_none() && kept.apps.is_empty());
        assert!(kept.homebrew.is_none() && kept.developer.is_none() && kept.aterm.is_none());
        let owned = format!(
            "{starter}\n[[app]]\nname = \"Caffeine\"\nkind = \"dmg-app\"\n\
             url = \"https://example.com/Caffeine.dmg\"\nsha256 = \"{}\"\n\
             team_id = \"YD6LEYT6WZ\"\nbundle = \"Caffeine.app\"\nversion = \"1.1.4\"\n",
            "0".repeat(64)
        );
        let both = parse(&keep_only(&owned, &["names".into(), "app".into()]).unwrap()).unwrap();
        assert!(both.names.is_some() && both.apps.len() == 1 && both.power.is_none());
        let text = "schema = 1\nusers = [\"admin\"]\n[names]\ncomputer_name = \"x\"\n\
                    [power.ac]\nsleep = 0\n";
        let kept = parse(&keep_only(text, &["power".into()]).unwrap()).unwrap();
        assert_eq!(kept.users, ["admin"]);
        assert!(kept.names.is_none() && kept.power.is_some());
        assert_eq!(keep_only(text, &[]).unwrap(), text);
    }

    #[test]
    fn placeholders_are_filled_and_unknown_ones_survive() {
        let text = "schema = 1\n[names]\ncomputer_name = \"{name}\"\n";
        let filled = fill(text, &[("name", "lab-07")]).unwrap();
        assert!(filled.contains("\"lab-07\""), "{filled}");
        assert_eq!(
            fill_str("{user}@{name}", &[("name", "lab-07")]),
            "{user}@lab-07"
        );
    }
}
