// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Evaluates `deny.toml` `[bans] deny` and `[sources]` policies over each cell's resolved graph.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aterm_toml::edit::{DocumentMut, Item};

use crate::attest::{Ver, parse_version};
use crate::model::{Graph, PkgId};

/// One `[bans] deny` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ban {
    pub name: String,
    /// A cargo-deny version requirement; `None` bans every version.
    pub version: Option<String>,
    /// The only packages allowed to depend on it directly; empty: none is.
    pub wrappers: Vec<String>,
    pub reason: String,
}

/// What `deny.toml` says about the resolved graph.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    pub bans: Vec<Ban>,
    /// `[sources] allow-git`: URL prefixes a git source may start with.
    pub allow_git: Vec<String>,
}

/// One violation in one cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finding {
    /// `pkg` matches a ban; `via` holds its non-wrapper parents (empty for the root).
    Banned {
        pkg: PkgId,
        via: Vec<PkgId>,
        ban: Ban,
    },
    /// `pkg` comes from `source`, which is neither crates.io, a path, nor an allowed git prefix.
    Source { pkg: PkgId, source: String },
    /// A ban's version requirement is not a form this check understands.
    BadRequirement { ban: Ban },
}

/// Read the policy from `<root>/deny.toml`.
///
/// # Errors
/// The file is missing or not TOML, or a ban entry is not an inline table or has no `name`.
pub fn policy(root: &Path) -> Result<Policy, String> {
    let path = root.join("deny.toml");
    let text = crate::read_regular_text(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse_policy(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// [`policy`] over the file's text.
///
/// # Errors
/// Not TOML, or a ban entry that is not an inline table or has no `name`.
pub fn parse_policy(text: &str) -> Result<Policy, String> {
    let doc: DocumentMut = text.parse().map_err(|e| format!("not valid TOML: {e}"))?;
    let strings = |item: Option<&Item>| -> Vec<String> {
        item.and_then(Item::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut bans = Vec::new();
    let deny = doc
        .get("bans")
        .and_then(Item::as_table_like)
        .and_then(|t| t.get("deny"))
        .and_then(Item::as_array);
    for entry in deny.into_iter().flat_map(|a| a.iter()) {
        let Some(table) = entry.as_inline_table() else {
            return Err("a `[bans] deny` entry is not an inline table".to_string());
        };
        let text = |key: &str| table.get(key).and_then(|v| v.as_str()).map(str::to_string);
        let name = text("name").ok_or("a `[bans] deny` entry has no `name`")?;
        let wrappers = table
            .get("wrappers")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        bans.push(Ban {
            name,
            version: text("version"),
            wrappers,
            reason: text("reason").unwrap_or_default(),
        });
    }
    let allow_git = strings(
        doc.get("sources")
            .and_then(Item::as_table_like)
            .and_then(|t| t.get("allow-git")),
    );
    Ok(Policy { bans, allow_git })
}

/// Every violation of `policy` in one cell's `graph` with cargo path `annotations`.
#[must_use]
pub fn findings(
    policy: &Policy,
    graph: &Graph,
    annotations: &BTreeMap<PkgId, PathBuf>,
) -> Vec<Finding> {
    let mut out = Vec::new();
    for ban in &policy.bans {
        let matches = |pkg: &PkgId| -> Option<bool> {
            if pkg.name != ban.name {
                return Some(false);
            }
            ban.version
                .as_deref()
                .map_or(Some(true), |req| satisfies(req, &pkg.version))
        };
        let mut ban_findings = Vec::new();
        let mut bad_req = false;
        for pkg in &graph.nodes {
            match matches(pkg) {
                None => {
                    bad_req = true;
                    break;
                }
                Some(false) => {}
                Some(true) => {
                    let via: Vec<PkgId> = graph
                        .edges
                        .iter()
                        .filter(|(_, children)| children.contains(pkg))
                        .map(|(parent, _)| parent.clone())
                        .filter(|parent| !ban.wrappers.contains(&parent.name))
                        .collect();
                    let rooted = *pkg == graph.root;
                    if rooted || !via.is_empty() {
                        ban_findings.push(Finding::Banned {
                            pkg: pkg.clone(),
                            via,
                            ban: ban.clone(),
                        });
                    }
                }
            }
        }
        if bad_req {
            out.push(Finding::BadRequirement { ban: ban.clone() });
        } else {
            out.extend(ban_findings);
        }
    }
    for (pkg, note) in annotations {
        let source = note.to_string_lossy();
        let is_dir = note.is_absolute() && note.is_dir();
        if !is_dir
            && !policy
                .allow_git
                .iter()
                .any(|g| source.starts_with(g.as_str()))
        {
            out.push(Finding::Source {
                pkg: pkg.clone(),
                source: source.into_owned(),
            });
        }
    }
    out
}

/// Whether `version` satisfies comma-separated comparators (`<`, `<=`, `>`, `>=`, `=`), or `None`
/// if unsupported.
#[must_use]
pub fn satisfies(req: &str, version: &str) -> Option<bool> {
    let have = parse_version(version)?;
    let mut all = true;
    for part in req.split(',') {
        let part = part.trim();
        let (op, rest) = ["<=", ">=", "<", ">", "="]
            .iter()
            .find_map(|op| part.strip_prefix(op).map(|rest| (*op, rest)))?;
        let want: Ver = parse_version(rest.trim())?;
        let (h, w) = (have.triple(), want.triple());
        all &= match op {
            "<=" => h <= w,
            ">=" => h >= w,
            "<" => h < w,
            ">" => h > w,
            _ => h == w,
        };
    }
    Some(all)
}

#[cfg(test)]
#[path = "deny_tests.rs"]
mod tests;
