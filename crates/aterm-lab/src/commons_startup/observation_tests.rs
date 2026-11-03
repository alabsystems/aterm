// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A lab observation must not repair the host whose startup it measures.

use super::*;

fn refuses_without_repair(observe: impl FnOnce(&Paths, &Path, &table::Table) -> bool) {
    let world = World::new(CONFIG_OFF).unwrap();
    let paths = Paths::for_home(&world.home);
    let vault = world.dir.join("observer.vault");
    client::create(&paths, vault.to_str().unwrap(), None, Some("source")).unwrap();
    client::seed_vault_genesis(&paths, &world.dir.join("observer.genesis")).unwrap();

    let table = table::Table::application(&paths, "observer-data", "items").unwrap();
    table.publish(&paths).unwrap();
    let (operation, value) = table.write(&paths, "present").unwrap();
    let config = world.cfg.join("aterm/aterm.toml");
    let (rows, known) = table.observe(&paths, &config).unwrap();
    assert_eq!(rows.get(&operation), Some(&value));
    assert_eq!(known, std::collections::BTreeSet::from([operation]));
    require_enrolled(&paths, true).unwrap();

    let lock = paths.dir.join("commons.lock");
    std::fs::remove_file(&lock).unwrap();
    let refused = observe(&paths, &config, &table);
    assert!(
        !lock.exists(),
        "a lab observation recreated missing custody"
    );
    assert!(refused, "missing custody was accepted by a lab observation");
}

#[test]
fn table_identity_observation_does_not_repair_custody() {
    refuses_without_repair(|paths, _, _| {
        table::Table::application(paths, "observer-data", "items").is_err()
    });
}

#[test]
fn table_contents_observation_does_not_repair_custody() {
    refuses_without_repair(|paths, config, table| table.observe(paths, config).is_err());
}

#[test]
fn enrollment_observation_does_not_repair_custody() {
    refuses_without_repair(|paths, _, _| require_enrolled(paths, true).is_err());
}
