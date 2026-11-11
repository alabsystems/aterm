// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    let mut two_versions = graph();
    two_versions.nodes.insert(id("ring", "not-semver"));
    two_versions
        .edges
        .get_mut(&id("aterm", "1.0.0"))
        .expect("root edges")
        .insert(id("ring", "not-semver"));
    let mixed = Policy {
        bans: vec![ban("ring", Some(">=0.17.14"), &[])],
        ..Policy::default()
    };
    assert_eq!(
        findings(&mixed, &two_versions, &BTreeMap::new()),
        [Finding::BadRequirement {
            ban: ban("ring", Some(">=0.17.14"), &[])
        }],
        "an unparseable version on a later node discards earlier Banned hits for the same ban"
    );
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
