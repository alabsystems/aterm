// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::bootstrap_input;
use crate::own_tests::{Files, Touched};
use std::collections::BTreeMap;

struct Fixture(BTreeMap<String, String>);

impl Files for Fixture {
    fn under(&self, dir: &str) -> Vec<String> {
        self.0
            .keys()
            .filter(|p| p.starts_with(dir))
            .cloned()
            .collect()
    }

    fn now(&self, path: &str) -> Result<Option<String>, String> {
        Ok(self.0.get(path).cloned())
    }

    fn before(&self, path: &str) -> Result<Option<String>, String> {
        if path == "crates/atpkg/src/cache_tests.rs" {
            Ok(Some("pub fn value() -> u8 { 1 }".into()))
        } else {
            self.now(path)
        }
    }
}

fn evidence(gate: &str, embeds: &str) -> Vec<String> {
    let fixture = Fixture(BTreeMap::from([
        (
            "crates/atpkg/Cargo.toml".into(),
            "[package]\nname = \"atpkg\"\nversion = \"0.0.0\"\n".into(),
        ),
        (
            "crates/atpkg/src/lib.rs".into(),
            format!("{gate}\n#[path = \"cache_tests.rs\"] mod tests;\n{embeds}"),
        ),
        (
            "crates/atpkg/src/cache_tests.rs".into(),
            "pub fn value() -> u8 { 2 }".into(),
        ),
    ]));
    crate::own_tests::confined(
        &fixture,
        "crates/atpkg",
        Touched {
            changed: &["crates/atpkg/src/cache_tests.rs"],
            readers: &[],
            elsewhere: &[],
        },
    )
    .is_ok()
    .then(|| "atpkg".into())
    .into_iter()
    .collect()
}

#[test]
fn only_proven_test_source_leaves_the_bootstrap_rig() {
    let path = "crates/atpkg/src/cache_tests.rs";
    let test_only = evidence("#[cfg(test)]", "");
    assert_eq!(test_only, ["atpkg"]);
    assert!(!bootstrap_input(path, &test_only));
    assert!(bootstrap_input(path, &[]));
    let production = evidence("", "");
    assert!(production.is_empty());
    assert!(bootstrap_input(path, &production));
    let embedded = evidence(
        "#[cfg(test)]",
        "const BYTES: &str = include_str!(\"cache_tests.rs\");",
    );
    assert!(embedded.is_empty());
    assert!(bootstrap_input(path, &embedded));
}

#[test]
fn source_evidence_cannot_exempt_other_bootstrap_inputs() {
    let confined = evidence("#[cfg(test)]", "");
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "trust.lock",
        "rust-toolchain.toml",
        "crates/atpkg/Cargo.toml",
        "crates/atpkg/.cargo/config.toml",
        "crates/atpkg/src/targo.sh",
        "tools/install.sh",
        "crates/aterm-toolchain/src/tool_tests.rs",
    ] {
        assert!(bootstrap_input(path, &confined), "{path}");
    }
}
