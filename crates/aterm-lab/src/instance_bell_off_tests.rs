// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn root_bell(text: &str) -> Option<bool> {
    aterm_toml::from_str::<aterm_toml::Value>(text)
        .ok()?
        .get("bell_sound")
        .and_then(aterm_toml::Value::as_bool)
}

#[test]
fn a_lab_world_starts_with_the_audible_bell_off() {
    assert_eq!(
        root_bell(CONFIG_OFF),
        None,
        "the base sets no bell_sound: aterm's default (on) would rule it",
    );
    let world = World::new(CONFIG_OFF).expect("fresh private world");
    let text = std::fs::read_to_string(world.cfg.join("aterm/aterm.toml")).expect("written");
    assert_eq!(root_bell(&text), Some(false), "{text}");
    let doc = aterm_toml::from_str::<aterm_toml::Value>(&text).expect("one document");
    assert_eq!(
        doc.get("update")
            .and_then(|t| t.get("enabled"))
            .and_then(aterm_toml::Value::as_bool),
        Some(false),
        "the lanes stay off: {text}",
    );
}

#[test]
fn a_bell_the_caller_sets_stands_alone() {
    let asked = config_with(&["bell_sound = true".to_string()]);
    assert!(
        aterm_toml::from_str::<aterm_toml::Value>(&format!("bell_sound = false\n{asked}")).is_err(),
        "a key written twice is no document",
    );
    assert_eq!(root_bell(&bell_off(&asked)), Some(true));
    assert_eq!(bell_off(&asked), asked, "the caller's file, unchanged");
}

#[test]
fn a_bell_sound_inside_a_table_is_not_the_root_key() {
    let text = bell_off("[x]\nbell_sound = true\n");
    assert_eq!(root_bell(&text), Some(false), "{text}");
    let doc = aterm_toml::from_str::<aterm_toml::Value>(&text).expect("one document");
    assert_eq!(
        doc.get("x")
            .and_then(|t| t.get("bell_sound"))
            .and_then(aterm_toml::Value::as_bool),
        Some(true),
    );
}
