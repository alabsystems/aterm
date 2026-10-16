// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness's own self-test, in ONE binary: `harness.rs` is mounted by both
//! suites, so a `#[test]` there would run once per mounting binary. What it
//! guards is the fixture config that isolates every world's aterm from the
//! machine.
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use crate::harness;

use std::process::Command;

use harness::{FIXTURE_GUI_CONFIG, prepare_fixture_config, prepare_gui_environment};

#[test]
fn gui_fixture_defaults_disable_machine_writes_and_preserve_explicit_edits() {
    let dir = std::env::temp_dir().join(format!("atl-isolation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    prepare_fixture_config(&dir);
    let path = dir.join("cfg/aterm/aterm.toml");
    let text = std::fs::read_to_string(&path).expect("fixture config");
    let config: aterm_toml::Table = aterm_toml::from_str(&text).expect("valid fixture config");
    assert_eq!(
        config.get("agents_auto_prime").and_then(|v| v.as_bool()),
        Some(false)
    );
    let packages = config.get("packages").and_then(|v| v.as_table()).unwrap();
    let machine = config.get("machine").and_then(|v| v.as_table()).unwrap();
    assert_eq!(
        packages.get("enabled").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        machine.get("universal_control").and_then(|v| v.as_str()),
        Some("leave")
    );

    let edited = format!("{text}\n[fabric]\npresence = \"minimal\"\n");
    std::fs::write(&path, &edited).expect("intentional fixture edit");
    prepare_fixture_config(&dir);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);

    // The updater is off through the config, not the environment (2026-09-23).
    let update = aterm_toml::from_str::<aterm_toml::Table>(FIXTURE_GUI_CONFIG)
        .expect("fixture config")
        .get("update")
        .and_then(|v| v.as_table())
        .cloned()
        .expect("[update]");
    assert_eq!(update.get("enabled").and_then(|v| v.as_bool()), Some(false));

    // It must run first on a fresh Command (its own doc says why), so it can only
    // REMOVE: every inherited ATERM_* is removed, and nothing is set in its place.
    let mut command = Command::new("unused-fixture-program");
    prepare_gui_environment(&mut command);
    assert!(
        command
            .get_envs()
            .filter(|(key, _)| key.to_string_lossy().starts_with("ATERM_"))
            .all(|(_, value)| value.is_none()),
        "the baseline sets no ATERM_* variable: isolation is the fixture config"
    );
    std::fs::remove_dir_all(dir).expect("remove owned fixture");
}
