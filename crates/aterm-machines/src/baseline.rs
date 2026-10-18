// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The baseline: the desired state of a host, one TOML document (design §5).
//!
//! Each family owns its table's type (`items::<family>::Table`), so this file only
//! composes them. Unknown keys are refused everywhere: a typo in a baseline must not
//! read as "leave that setting alone".

use crate::items;
use serde::Deserialize;

/// A whole baseline.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    /// [`crate::SCHEMA`].
    pub schema: u32,
    /// The accounts user-scope items apply to. The controller fills it from the
    /// inventory's `users` (default: the host's SSH account) when the baseline leaves it out.
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
///
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

/// Fill `{key}` placeholders in every string of a baseline document from `vars`, and
/// return the document re-serialized. A placeholder `vars` does not name is left alone,
/// so per-user placeholders (`{user}`) survive for the items to fill.
///
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
            // aterm-toml's map has no `iter_mut`: take each value out and put it back.
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
