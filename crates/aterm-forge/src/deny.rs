// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Evaluates `deny.toml` `[bans]` and `[sources]` policies over each cell's resolved graph.

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
    let text = std::fs::read_to_string(&path)
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
        for pkg in &graph.nodes {
            match matches(pkg) {
                None => {
                    out.push(Finding::BadRequirement { ban: ban.clone() });
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
                        out.push(Finding::Banned {
                            pkg: pkg.clone(),
                            via,
                            ban: ban.clone(),
                        });
                    }
                }
            }
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

/// Whether `version` satisfies comma-separated comparators (`<`, `<=`, `>`, `>=`, `=`).
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
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn id(name: &str, version: &str) -> PkgId {
        PkgId::new(name, version)
    }

    fn graph() -> Graph {
        let root = id("aterm", "1.0.0");
        let (app, ring, rng) = (
            id("aterm-app", "1.0.0"),
            id("ring", "0.17.14"),
            id("getrandom", "0.2.17"),
        );
        let mut edges = BTreeMap::new();
        edges.insert(root.clone(), BTreeSet::from([app.clone(), ring.clone()]));
        edges.insert(app.clone(), BTreeSet::from([rng.clone()]));
        edges.insert(ring.clone(), BTreeSet::from([rng.clone()]));
        Graph {
            nodes: BTreeSet::from([root.clone(), app, ring, rng]),
            root,
            edges,
        }
    }

    fn ban(name: &str, version: Option<&str>, wrappers: &[&str]) -> Ban {
        Ban {
            name: name.to_string(),
            version: version.map(str::to_string),
            wrappers: wrappers.iter().map(|w| (*w).to_string()).collect(),
            reason: "why".to_string(),
        }
    }

    #[test]
    fn a_ban_names_every_parent_that_is_not_a_wrapper() {
        let policy = Policy {
            bans: vec![ban("getrandom", None, &["ring"])],
            ..Policy::default()
        };
        let got = findings(&policy, &graph(), &BTreeMap::new());
        assert_eq!(
            got,
            [Finding::Banned {
                pkg: id("getrandom", "0.2.17"),
                via: vec![id("aterm-app", "1.0.0")],
                ban: ban("getrandom", None, &["ring"]),
            }]
        );
        let wrapped = Policy {
            bans: vec![ban("getrandom", None, &["ring", "aterm-app"])],
            ..Policy::default()
        };
        assert!(findings(&wrapped, &graph(), &BTreeMap::new()).is_empty());
    }

    #[test]
    fn a_version_requirement_bans_only_the_versions_it_matches() {
        let below = Policy {
            bans: vec![ban("ring", Some("<0.17.14"), &[])],
            ..Policy::default()
        };
        assert!(findings(&below, &graph(), &BTreeMap::new()).is_empty());
        let at = Policy {
            bans: vec![ban("ring", Some(">=0.17.14"), &[])],
            ..Policy::default()
        };
        assert_eq!(findings(&at, &graph(), &BTreeMap::new()).len(), 1);
        let unread = Policy {
            bans: vec![ban("ring", Some("~0.17"), &[])],
            ..Policy::default()
        };
        assert!(matches!(
            findings(&unread, &graph(), &BTreeMap::new())[..],
            [Finding::BadRequirement { .. }]
        ));
    }

    #[test]
    fn a_source_is_a_directory_or_an_allowed_git_prefix() {
        let dir = std::env::temp_dir();
        let mut notes = BTreeMap::new();
        notes.insert(id("aterm-app", "1.0.0"), dir);
        notes.insert(
            id("ring", "0.17.14"),
            PathBuf::from("https://github.com/briansmith/ring#a1b2c3"),
        );
        let closed = Policy::default();
        assert_eq!(
            findings(&closed, &graph(), &notes),
            [Finding::Source {
                pkg: id("ring", "0.17.14"),
                source: "https://github.com/briansmith/ring#a1b2c3".to_string(),
            }]
        );
        let open = Policy {
            allow_git: vec!["https://github.com/briansmith/".to_string()],
            ..Policy::default()
        };
        assert!(findings(&open, &graph(), &notes).is_empty());
    }

    #[test]
    fn the_policy_reads_bans_wrappers_and_git_sources() {
        let policy = parse_policy(
            r#"
[bans]
deny = [
    { name = "ring", version = "<0.17.14", reason = "floor" },
    { name = "getrandom", wrappers = ["ring"], reason = "entropy" },
]
[sources]
allow-git = ["https://example.org/"]
"#,
        )
        .expect("parses");
        assert_eq!(
            policy.bans,
            [
                Ban {
                    name: "ring".to_string(),
                    version: Some("<0.17.14".to_string()),
                    wrappers: vec![],
                    reason: "floor".to_string(),
                },
                ban("getrandom", None, &["ring"]).with_reason("entropy"),
            ]
        );
        assert_eq!(policy.allow_git, ["https://example.org/"]);
        assert!(parse_policy("[bans]\ndeny = [{ version = \"1\" }]\n").is_err());
    }

    impl Ban {
        fn with_reason(mut self, reason: &str) -> Self {
            self.reason = reason.to_string();
            self
        }
    }

    #[test]
    fn comparators_compose_with_commas() {
        assert_eq!(satisfies("<0.17.14", "0.17.13"), Some(true));
        assert_eq!(satisfies("<0.17.14", "0.17.14"), Some(false));
        assert_eq!(satisfies(">=0.2, <0.3", "0.2.17"), Some(true));
        assert_eq!(satisfies(">=0.2, <0.3", "0.3.4"), Some(false));
        assert_eq!(satisfies("=1.2.3", "1.2.3"), Some(true));
        assert_eq!(satisfies("^1", "1.2.3"), None);
    }
}
