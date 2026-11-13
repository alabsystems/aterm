// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
