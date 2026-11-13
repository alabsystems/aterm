// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one rainbow theme and the pet beside it: an aterm.toml's cursor settings migrate once
//! (old names renamed, the pet each cursor had kept, nobody gaining one) and old names are
//! refused afterwards, a fresh config draws the rainbow beside the cat, and the pet stays
//! beside every style and beside none.

use super::*;
use crate::app_config::{Config, resolve_trail_presentation, resolve_trail_style};
use aterm_effects::cursor_glow::spellings::{self, RETIRED_TRAIL_STYLES, TRAIL_STYLES};
use aterm_effects::cursor_pet::PETS;
use aterm_effects::kitty_pet::PetSpecies;

fn config(text: &str) -> Config {
    aterm_toml::from_str(text).expect("valid toml")
}

fn value<'a>(doc: &'a aterm_toml::edit::DocumentMut, key: &str) -> Option<&'a str> {
    doc.get(key).and_then(aterm_toml::edit::Item::as_str)
}

fn parsed(text: &str) -> aterm_toml::edit::DocumentMut {
    text.parse().expect("the rewritten file parses")
}

/// Every name the old table took (its five rainbow rows and every alias, each `nyan` among
/// them) is rewritten to the style it became under both keys that held one, with the pet its
/// cursor had; the rewritten file is changed by no second pass; and the name is refused
/// wherever a setting is written or read.
#[test]
fn old_trail_names_migrate_once_then_are_refused() {
    assert!(
        RETIRED_TRAIL_STYLES.len() >= 38,
        "the old table's 5 rainbow rows, their 22 aliases and the 11 other aliases"
    );
    for was in RETIRED_TRAIL_STYLES {
        // The map's own law: an old name is no style now, and what it became is one.
        assert!(resolve_trail_style(was.old).unknown, "{:?}", was.old);
        assert!(!resolve_trail_style(was.style).unknown, "{:?}", was.old);
        if let Some(pet) = was.pet {
            assert!(PETS.iter().any(|row| row.name == pet), "{:?}", was.old);
        }
        for key in [EDIT_CURSOR_TRAIL_STYLE, EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF] {
            // The name as a file may spell it: padded and in capitals.
            let spelled = format!("  {}  ", was.old.to_ascii_uppercase());
            let before = format!("font_size = 14\n{key} = {spelled:?} # mine\nmotion = \"full\"\n");
            let migration = migrate_cursor_settings(&before)
                .unwrap_or_else(|| panic!("{key} = {:?} is migrated", was.old));
            let after = &migration.text;
            let doc = parsed(after);
            assert_eq!(value(&doc, key), Some(was.style), "{after}");
            // The style drawn keeps the pet it had: its animal, or none. The style Off keeps
            // names no pet.
            let pet = (key == EDIT_CURSOR_TRAIL_STYLE).then_some(was.pet.unwrap_or("off"));
            assert_eq!(value(&doc, EDIT_CURSOR_PET), pet, "{after}");
            assert_eq!(migration.pet, pet, "{after}");
            assert!(
                after.contains("# mine"),
                "the line keeps its comment: {after}"
            );
            assert!(after.contains("font_size = 14") && after.contains("motion = \"full\""));
            assert_eq!(
                migration.renamed,
                [RetiredCursorName {
                    key,
                    old: spelled.clone(),
                    style: was.style,
                }]
            );
            // Once: the rewritten file is not rewritten again.
            assert_eq!(migrate_cursor_settings(after), None, "{after}");
        }
        // Refused: a write of the old name changes no file and names the new settings.
        for key in [EDIT_CURSOR_TRAIL_STYLE, EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF] {
            let refused = apply_prefs_edits("", &[(key, Some(was.old.to_string()))])
                .expect_err("a retired name is refused");
            let said = refused.to_string();
            assert!(
                matches!(refused, PrefsEditError::RetiredName { .. }),
                "{said}"
            );
            assert!(said.contains(&format!("{key} = {:?}", was.style)), "{said}");
            assert_eq!(
                said.contains(EDIT_CURSOR_PET),
                was.pet.is_some(),
                "an animal's name says its pet: {said}"
            );
        }
        // Refused: a name typed after the migration draws the default and says what to set.
        let held = config(&format!("{EDIT_CURSOR_TRAIL_STYLE} = {:?}\n", was.old));
        let warning = held
            .cursor_trail_style_warning()
            .unwrap_or_else(|| panic!("{:?} warns", was.old));
        assert!(warning.contains("no longer a style"), "{warning}");
        assert!(
            warning.contains(&format!("{EDIT_CURSOR_TRAIL_STYLE} = {:?}", was.style)),
            "{warning}"
        );
        assert_eq!(cursor_trail_style_canonical(was.old), None);
        assert!(!CURSOR_TRAIL_STYLES.contains(&was.old), "{:?}", was.old);
    }
    // No row and no pet is an old name, and no accepted name holds the word the brand retired.
    for name in TRAIL_STYLES
        .iter()
        .map(|row| row.name)
        .chain(PETS.iter().map(|row| row.name))
    {
        assert!(!name.contains("nyan"), "{name}");
        assert_eq!(spellings::retired(name), None, "{name}");
    }
}

/// Nobody gains an animal by upgrading: a file whose cursor had no pet (the plain cursor, the
/// retired master off, Fire and every other style that named no animal) is written
/// `cursor_pet = "off"`, whatever the style Off keeps says.
#[test]
fn a_cursor_that_had_no_pet_gains_none() {
    for row in TRAIL_STYLES {
        let before = format!("cursor_trail_style = {:?}\n", row.name);
        let migration = migrate_cursor_settings(&before);
        if row.name == DEFAULT_CURSOR_TRAIL_STYLE {
            // The theme's own name is a file written since: it keeps the default pet.
            assert_eq!(migration, None);
            continue;
        }
        let migration = migration.unwrap_or_else(|| panic!("{before} is migrated"));
        assert_eq!(migration.renamed, [], "{before}");
        assert_eq!(migration.pet, Some("off"), "{before}");
        let drawn = config(&migration.text);
        assert_eq!(
            (drawn.cursor_trail_style_raw(), drawn.cursor_pet_raw()),
            (row.name, "off"),
            "{before}"
        );
        assert_eq!(migrate_cursor_settings(&migration.text), None, "{before}");
    }
    // The worst case: the cursor turned off, with a dog's style kept for later. No dog walks
    // beside the plain cursor.
    let migration = migrate_cursor_settings(
        "cursor_trail_style = \"off\"\ncursor_trail_style_before_off = \"rainbow dog\"\n",
    )
    .expect("a retired name under Off");
    let doc = parsed(&migration.text);
    assert_eq!(value(&doc, EDIT_CURSOR_TRAIL_STYLE), Some("off"));
    assert_eq!(
        value(&doc, EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF),
        Some("rainbow")
    );
    assert_eq!(value(&doc, EDIT_CURSOR_PET), Some("off"));
    assert_eq!(config(&migration.text).cursor_pet_raw(), "off");
    assert_eq!(
        migration.sentences(),
        [
            "cursor_trail_style_before_off \"rainbow dog\" is now \"rainbow\"",
            "cursor_pet is now its own setting, written \"off\": this cursor had no pet",
        ]
    );
    // The retired master off drew nothing: off is written, beside any style.
    for style in ["", "cursor_trail_style = \"rainbow kitty pet\"\n"] {
        let before = format!("cursor_trail = false\n{style}");
        let migration = migrate_cursor_settings(&before).expect("the master off");
        assert_eq!(migration.pet, Some("off"), "{before}");
        assert_eq!(config(&migration.text).cursor_pet_raw(), "off", "{before}");
    }
    // A retired name that named no animal: Fire stays Fire, with no pet.
    let migration = migrate_cursor_settings("cursor_trail_style = \"embers\"\n").expect("an alias");
    let drawn = config(&migration.text);
    assert_eq!(
        (drawn.cursor_trail_style_raw(), drawn.cursor_pet_raw()),
        ("fire", "off")
    );
}

/// Nobody loses an animal either, and the file's own words about its pet stand.
#[test]
fn the_migration_keeps_the_pet_a_cursor_had() {
    // The style drawn names the pet; the style Off keeps is renamed and names none.
    let migration = migrate_cursor_settings(
        "cursor_trail_style = \"rainbow dog pet\"\ncursor_trail_style_before_off = \"nyan\"\n",
    )
    .expect("two retired names");
    let doc = parsed(&migration.text);
    assert_eq!(value(&doc, EDIT_CURSOR_TRAIL_STYLE), Some("rainbow"));
    assert_eq!(
        value(&doc, EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF),
        Some("rainbow")
    );
    assert_eq!(value(&doc, EDIT_CURSOR_PET), Some("dog"));
    assert_eq!(
        migration.sentences(),
        [
            "cursor_trail_style \"rainbow dog pet\" is now \"rainbow\"",
            "cursor_trail_style_before_off \"nyan\" is now \"rainbow\"",
            "cursor_pet is now its own setting, written \"dog\"",
        ]
    );
    // The flying style keeps its flight: the flying cat is a pet.
    for flying in [
        "rainbow kitty flying",
        "flying kitty",
        "nyan",
        "nyan rainbow",
    ] {
        let migration = migrate_cursor_settings(&format!("cursor_trail_style = {flying:?}\n"))
            .expect("a flying name");
        let drawn = config(&migration.text);
        assert_eq!(
            (drawn.cursor_trail_style_raw(), drawn.cursor_pet_raw()),
            ("rainbow", "flying cat"),
            "{flying}"
        );
        let look = resolve_trail_presentation("rainbow", drawn.cursor_pet_raw());
        assert_eq!((look.pet_species, look.pet_flies), (None, true), "{flying}");
    }
    // The retired master on with no style drew the default style and its cat on every
    // platform, Windows included: the cat is written, and drawn before it is.
    let migration = migrate_cursor_settings("cursor_trail = true\n").expect("the master on");
    assert_eq!(migration.pet, Some("cat"));
    assert_eq!(
        crate::app_config::drawn_cursor_pet(Some(true), None),
        "cat",
        "the master on keeps the cat where the platform default is none"
    );
    assert_eq!(
        crate::app_config::drawn_cursor_trail_style(Some(true), None),
        DEFAULT_CURSOR_TRAIL_STYLE
    );
    // A name that was no style drew the default style, which had the cat.
    let migration =
        migrate_cursor_settings("cursor_trail_style = \"xyzzy\"\n").expect("an unknown style");
    assert_eq!((migration.renamed.len(), migration.pet), (0, Some("cat")));
    // A pet the file already names is not overwritten.
    let migration = migrate_cursor_settings(
        "cursor_pet = \"off\"\ncursor_trail_style = \"rainbow kitty pet\"\n",
    )
    .expect("a retired name");
    assert_eq!(
        value(&parsed(&migration.text), EDIT_CURSOR_PET),
        Some("off")
    );
    assert_eq!(migration.pet, None);
    // A file that names neither a style nor the master draws the defaults, and is left alone;
    // so is a table's own key of the name, and a Classic face, which is not retired.
    for untouched in [
        "",
        "font_size = 14\n",
        "cursor_pet = \"dog\"\ncursor_trail_style = \"fire\"\n",
        "[render]\ncursor_trail_style = \"nyan\"\n",
        "cursor_pet = \"cat\"\ncursor_trail_style = \"classic mono\"\n",
    ] {
        assert_eq!(migrate_cursor_settings(untouched), None, "{untouched}");
    }
    // A file that does not parse is left for the reader that reports it.
    assert_eq!(
        migrate_cursor_settings("cursor_trail_style = \"nyan\n"),
        None
    );
}

#[cfg(unix)]
mod on_disk {
    use super::*;
    use crate::native_config_service::{
        ConfigDiskObservation, CursorMigrationStamp, VersionedConfigService,
        admit_cursor_migration, observe_through_cursor_migration,
    };
    use std::os::unix::fs::PermissionsExt;

    struct World {
        _scratch: aterm_tempfile::TempDir,
        dir: std::path::PathBuf,
        path: std::path::PathBuf,
        stamp: CursorMigrationStamp,
    }

    impl World {
        fn holding(text: &str) -> Self {
            let scratch = aterm_tempfile::tempdir().expect("scratch folder");
            let dir = scratch.path().join("config");
            std::fs::create_dir(&dir).expect("the config folder");
            let path = dir.join("aterm.toml");
            std::fs::write(&path, text).expect("the old file");
            let stamp = CursorMigrationStamp::at(scratch.path().join("state").join("stamp"));
            Self {
                _scratch: scratch,
                dir,
                path,
                stamp,
            }
        }

        fn observe(&self) -> ConfigDiskObservation {
            VersionedConfigService::observe_path(&self.path, false).expect("the file is read")
        }

        /// One launch or watcher read.
        fn read(&self) -> ConfigDiskObservation {
            observe_through_cursor_migration(&self.stamp, || Ok::<_, String>(self.observe()))
                .expect("the file is read")
        }

        fn on_disk(&self) -> String {
            std::fs::read_to_string(&self.path).expect("the file")
        }
    }

    /// The file is rewritten at its first read, the stamp records it, and a second read
    /// changes nothing.
    #[test]
    fn a_file_naming_an_old_style_is_rewritten_at_its_first_read() {
        let world =
            World::holding("# mine\ncursor_trail_style = \"rainbow dog pet\"\ncolumns = 100\n");
        assert!(!world.stamp.ran());
        let admitted = world.read();
        let migrated =
            "# mine\ncursor_trail_style = \"rainbow\"\ncolumns = 100\ncursor_pet = \"dog\"\n";
        assert_eq!(world.on_disk(), migrated);
        assert_eq!(
            admitted.text, migrated,
            "the observation is the file as written"
        );
        assert!(world.stamp.ran(), "the migration is recorded");
        let again = world.read();
        assert_eq!(
            (again.text.as_str(), world.on_disk().as_str()),
            (migrated, migrated)
        );
        let drawn = config(migrated);
        assert_eq!(drawn.cursor_trail_style_warning(), None);
        assert_eq!(
            (drawn.cursor_trail_style_raw(), drawn.cursor_pet_raw()),
            ("rainbow", "dog")
        );
    }

    /// The migration is once: after it ran, an old name typed into the file is not accepted
    /// and not rewritten. It is refused with the new setting named. A file with nothing to
    /// migrate is stamped by its first read just the same.
    #[test]
    fn an_old_name_typed_after_the_migration_is_refused_and_not_rewritten() {
        for first in ["cursor_trail_style = \"nyan\"\n", "columns = 100\n", ""] {
            let world = World::holding(first);
            world.read();
            assert!(world.stamp.ran(), "{first:?}");
            for typed in [
                "cursor_trail_style = \"rainbow dog pet\"\n",
                "cursor_trail_style = \"embers\"\n",
                "cursor_trail_style = \"off\"\n",
            ] {
                std::fs::write(&world.path, typed).expect("typed later");
                let read = world.read();
                assert_eq!(read.text, typed, "read as typed");
                assert_eq!(world.on_disk(), typed, "and not rewritten");
            }
            let refused = config("cursor_trail_style = \"embers\"\n");
            let warning = refused.cursor_trail_style_warning().expect("refused");
            assert!(
                warning.contains("no longer a style")
                    && warning.contains("cursor_trail_style = \"fire\""),
                "{warning}"
            );
            // A style typed later keeps the pet setting's default: the pet is its own setting.
            assert_eq!(
                config("cursor_trail_style = \"off\"\n").cursor_pet_raw(),
                crate::app_config::default_cursor_pet()
            );
        }
        // A file that does not parse is not the migration's to stamp: it waits for one it
        // can read.
        let world = World::holding("cursor_trail_style = \"nyan\n");
        world.read();
        assert!(!world.stamp.ran());
        std::fs::write(&world.path, "cursor_trail_style = \"nyan\"\n").expect("mended");
        assert_eq!(
            world.read().text,
            "cursor_trail_style = \"rainbow\"\ncursor_pet = \"flying cat\"\n"
        );
    }

    /// A file aterm cannot write (a managed or read-only aterm.toml) keeps the person's
    /// choice: each read draws what the file meant, the file is untouched, and the migration
    /// lands at the first read that can write.
    #[test]
    fn a_file_that_cannot_be_written_still_draws_what_it_meant() {
        for (old, style, pet) in [
            ("embers", "fire", "off"),
            ("rainbow dog pet", "rainbow", "dog"),
            ("lightning", "laser", "off"),
        ] {
            let text = format!("cursor_trail_style = {old:?}\n");
            let world = World::holding(&text);
            let lock = |mode| {
                std::fs::set_permissions(&world.dir, std::fs::Permissions::from_mode(mode))
                    .expect("the folder's mode");
                std::fs::set_permissions(
                    &world.path,
                    std::fs::Permissions::from_mode(mode & 0o666),
                )
                .expect("the file's mode");
            };
            lock(0o555);
            for _ in 0..2 {
                let read = world.read();
                assert_eq!(world.on_disk(), text, "{old}: the file is untouched");
                let drawn = config(&read.text);
                assert_eq!(drawn.cursor_trail_style_warning(), None, "{old}");
                assert_eq!(
                    (drawn.cursor_trail_style_raw(), drawn.cursor_pet_raw()),
                    (style, pet),
                    "{old}: drawn as the file meant"
                );
                assert!(!world.stamp.ran(), "{old}: the migration has not landed");
            }
            lock(0o755);
            let read = world.read();
            assert_eq!(world.on_disk(), read.text, "{old}: written once it can be");
            assert!(world.stamp.ran(), "{old}");
            assert_eq!(config(&read.text).cursor_trail_style_raw(), style, "{old}");
        }
    }

    /// Two aterms on one file: the one whose write loses still draws the migrated text, raises
    /// no refusal, and leaves the winner's file alone; and one that reads after the other's
    /// stamp sees a migrated file, never an old name typed later.
    #[test]
    fn the_loser_of_two_migrations_draws_the_migrated_file() {
        let world = World::holding("cursor_trail_style = \"rainbow dog pet\"\n");
        let stale = world.observe();
        // The other aterm migrates, writes and stamps between this one's read and write.
        let winner = world.read();
        assert!(world.stamp.ran());
        let unstamped = CursorMigrationStamp::at(world.dir.join("not-yet"));
        let loser = admit_cursor_migration(stale, &unstamped);
        assert_eq!(loser.text, winner.text, "the loser draws the migrated text");
        assert_eq!(config(&loser.text).cursor_trail_style_warning(), None);
        assert_eq!(world.on_disk(), winner.text, "the winner's file stands");
        assert!(!unstamped.ran(), "a lost write records nothing");
    }
}

/// A config that says nothing draws the one rainbow theme beside the cat where decorative
/// effects default on, and neither where they default off (Windows).
#[test]
fn a_fresh_config_draws_the_rainbow_beside_the_cat() {
    assert_eq!(DEFAULT_CURSOR_TRAIL_STYLE, "rainbow");
    assert_eq!(DEFAULT_CURSOR_PET, "cat");
    let fresh = config("");
    let (style, pet) = if crate::app_config::DEFAULT_DECORATIVE_EFFECTS {
        ("rainbow", "cat")
    } else {
        ("off", "off")
    };
    assert_eq!(
        (fresh.cursor_trail_style_raw(), fresh.cursor_pet_raw()),
        (style, pet)
    );
    assert_eq!(crate::app_config::default_cursor_pet(), pet);
    assert_eq!(
        (
            fresh.cursor_trail_style_warning(),
            fresh.cursor_pet_warning()
        ),
        (None, None)
    );
    // The theme's shape is the tall band with its line under the words, and one row names it.
    let rainbow = resolve_trail_presentation("rainbow", "cat");
    assert!(rainbow.ribbon_tall && !rainbow.ribbon_flat);
    assert_eq!(rainbow.pet_species, Some(PetSpecies::Cat));
    let rainbows = TRAIL_STYLES
        .iter()
        .filter(|row| row.style == Some(aterm_effects::cursor_glow::GlowStyle::RainbowKitty))
        .count();
    assert_eq!(rainbows, 1);
    assert_eq!(CURSOR_PETS, ["off", "cat", "dog", "flying cat"]);
}

/// The pet is its own setting: each animal stays beside every style and beside the plain
/// cursor, and `off` takes it from every one, the rainbow included.
#[test]
fn the_pet_stays_beside_every_style_and_the_plain_cursor() {
    for row in TRAIL_STYLES {
        for (pet, species) in [
            ("off", None),
            ("cat", Some(PetSpecies::Cat)),
            ("dog", Some(PetSpecies::Dog)),
        ] {
            let text = format!(
                "cursor_trail_style = {:?}\ncursor_pet = {pet:?}\n",
                row.name
            );
            let held = config(&text);
            assert_eq!(held.cursor_pet_warning(), None, "{text}");
            let drawn =
                resolve_trail_presentation(held.cursor_trail_style_raw(), held.cursor_pet_raw());
            assert_eq!(drawn.pet_species, species, "{text}");
            assert_eq!(drawn.style.style, row.style, "{text}");
        }
    }
    // A pet written through the settings door lands as its row's name, whatever the style.
    let after = apply_prefs_edits(
        "cursor_trail_style = \"off\"\n",
        &[(EDIT_CURSOR_PET, Some("Dog".to_string()))],
    )
    .expect("a pet beside the plain cursor");
    let doc = parsed(&after);
    assert_eq!(value(&doc, EDIT_CURSOR_PET), Some("dog"));
    assert_eq!(value(&doc, EDIT_CURSOR_TRAIL_STYLE), Some("off"));
    // A name that is no pet is refused at the door and warned of in a file.
    assert!(apply_prefs_edits("", &[(EDIT_CURSOR_PET, Some("kitty".to_string()))]).is_err());
    let unknown = config("cursor_pet = \"parrot\"\n");
    assert!(
        unknown
            .cursor_pet_warning()
            .is_some_and(|warning| warning.contains("off|cat|dog|flying cat")),
    );
}
