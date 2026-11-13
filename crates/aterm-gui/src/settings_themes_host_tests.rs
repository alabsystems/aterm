// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aterm-gui-themes-host-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Only a valid id that names a regular file in the folder resolves; a
/// traversal spelling or a missing file names nothing.
#[test]
fn theme_file_resolves_only_a_regular_file_by_a_valid_id() {
    let dir = scratch("file");
    std::fs::write(dir.join("mine.conf"), "background = #000000\n").unwrap();
    assert_eq!(theme_file(&dir, "mine"), Some(dir.join("mine.conf")));
    assert_eq!(theme_file(&dir, "absent"), None);
    assert_eq!(theme_file(&dir, "../mine"), None);
    assert_eq!(theme_file(&dir, "a/b"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Save as theme writes a file the catalog admits under that id, never
/// over an existing theme, and refuses a built-in's name or a bad id
/// before writing anything.
#[test]
fn save_writes_a_new_theme_once_and_refuses_bad_names() {
    let dir = scratch("save");
    let text = aterm_types::scheme_import::to_native_conf(
        &aterm_types::ColorScheme {
            name: "Harbour Custom".to_string(),
            ..aterm_types::ColorScheme::default()
        },
        true,
        Some("saved"),
        None,
        &[],
    );
    assert_eq!(
        save_into(&dir, "Harbour Custom", &text),
        ThemesOutcome::Saved {
            id: "Harbour Custom".to_string()
        }
    );
    let catalog = crate::app_config::ThemeCatalog::discover_in(&dir);
    assert!(catalog.ready_names().any(|name| name == "Harbour Custom"));
    assert!(matches!(
        save_into(&dir, "Harbour Custom", &text),
        ThemesOutcome::Failed { message } if message.contains("already")
    ));
    for bad in ["Nord", "a/b", "dark:x", ""] {
        assert!(
            matches!(save_into(&dir, bad, &text), ThemesOutcome::Failed { .. }),
            "{bad:?}"
        );
    }
    assert!(matches!(
        save_into(&dir, "garbage", "this is not a theme = ="),
        ThemesOutcome::Failed { .. }
    ));
    assert!(!dir.join("garbage.conf").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Trash moves a theme of the person's own out of the folder (the catalog
/// no longer lists it), and refuses a built-in's name, a missing file and
/// a symlink without calling the mover.
#[test]
fn trash_moves_only_a_regular_theme_file_of_the_persons_own() {
    fn refuse(_: &std::path::Path) -> Result<(), String> {
        panic!("the mover must not run for a refused id")
    }
    fn aside(file: &std::path::Path) -> Result<(), String> {
        let dest = file.with_extension("trashed");
        std::fs::rename(file, dest).map_err(|error| error.to_string())
    }
    let dir = scratch("trash");
    std::fs::write(dir.join("mine.conf"), "background = #000000\n").unwrap();
    assert!(matches!(
        trash_from(&dir, "Nord", refuse),
        ThemesOutcome::Failed { .. }
    ));
    assert!(matches!(
        trash_from(&dir, "absent", refuse),
        ThemesOutcome::Failed { .. }
    ));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.join("mine.conf"), dir.join("link.conf")).unwrap();
        assert!(matches!(
            trash_from(&dir, "link", refuse),
            ThemesOutcome::Failed { .. }
        ));
        std::fs::remove_file(dir.join("link.conf")).unwrap();
    }
    assert_eq!(
        trash_from(&dir, "mine", aside),
        ThemesOutcome::Trashed {
            id: "mine".to_string()
        }
    );
    let catalog = crate::app_config::ThemeCatalog::discover_in(&dir);
    assert!(!catalog.ready_names().any(|name| name == "mine"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A HEADLESS INSTANCE'S REACHES STOP AT ITS THEMES FOLDER: its Trash
/// moves the file into the `.trash` folder beside the themes (the
/// catalog no longer lists it, the bytes are still there), and Open and
/// Import say they are not done here, the folder untouched. Negative
/// control: a windowed instance gets the live arms — the system Trash,
/// the platform's open and its picker.
#[test]
fn a_headless_instance_trashes_into_its_own_folder_and_opens_nothing() {
    fn same(a: ThemesHostArms, b: ThemesHostArms) -> bool {
        std::ptr::fn_addr_eq(a.trash, b.trash)
            && std::ptr::fn_addr_eq(a.open, b.open)
            && std::ptr::fn_addr_eq(a.choose, b.choose)
    }
    assert!(same(
        ThemesHostArms::for_instance(false),
        ThemesHostArms::live()
    ));
    assert!(same(
        ThemesHostArms::for_instance(true),
        ThemesHostArms::isolated()
    ));
    let live = ThemesHostArms::live();
    let isolated = ThemesHostArms::isolated();
    assert!(!std::ptr::fn_addr_eq(live.trash, isolated.trash));
    assert!(!std::ptr::fn_addr_eq(live.open, isolated.open));
    assert!(!std::ptr::fn_addr_eq(live.choose, isolated.choose));

    let dir = scratch("headless");
    let text = "background = #000000\nforeground = #ffffff\n";
    std::fs::write(dir.join("mine.conf"), text).unwrap();
    let arms = ThemesHostArms::for_instance(true);
    assert_eq!(
        perform_in(
            &dir,
            &ThemesRequest::Trash {
                id: "mine".to_string()
            },
            arms
        ),
        ThemesOutcome::Trashed {
            id: "mine".to_string()
        }
    );
    assert!(!dir.join("mine.conf").exists());
    let kept: Vec<String> = walk(&dir.join(".trash"))
        .into_iter()
        .filter_map(|file| std::fs::read_to_string(file).ok())
        .collect();
    assert_eq!(kept, vec![text.to_string()], "the bytes stay in the folder");
    let catalog = crate::app_config::ThemeCatalog::discover_in(&dir);
    assert!(!catalog.ready_names().any(|name| name == "mine"));
    let refused_open = (isolated.open)(&dir).expect_err("the isolated open refuses");
    let refused_choose = (isolated.choose)().expect_err("the isolated picker refuses");
    for (request, refusal) in [
        (ThemesRequest::OpenFolder, &refused_open),
        (
            ThemesRequest::OpenFile {
                id: "mine".to_string(),
            },
            &refused_open,
        ),
        (ThemesRequest::Import, &refused_choose),
    ] {
        assert_eq!(
            perform_in(&dir, &request, arms),
            ThemesOutcome::Failed {
                message: refusal.clone()
            },
            "{request:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every regular file under `dir`, at any depth.
fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}

/// The window's import writes a new theme the catalog then lists, and
/// a second import of the same file refuses instead of overwriting.
#[test]
fn import_writes_a_theme_the_catalog_lists_and_never_overwrites() {
    let root = scratch("import");
    let themes = root.join("themes");
    let source = root.join("harbour.conf");
    std::fs::write(&source, "foreground #c0caf5\nbackground #1a1b26\n").unwrap();
    assert_eq!(
        import_from(&source, &themes),
        ThemesOutcome::Imported {
            id: "harbour".to_string(),
            dropped: 0
        }
    );
    let catalog = crate::app_config::ThemeCatalog::discover_in(&themes);
    assert!(catalog.ready_names().any(|name| name == "harbour"));
    assert!(matches!(
        import_from(&source, &themes),
        ThemesOutcome::Failed { message } if message.contains("already")
    ));
    let _ = std::fs::remove_dir_all(&root);
}

/// A community add installs the pack's file; again it is already there
/// (still added); an unknown id or an edited file of that name fails
/// and leaves the file alone.
#[test]
fn add_pack_installs_once_and_keeps_an_edited_file() {
    let dir = scratch("add-pack");
    let added = ThemesOutcome::Added {
        id: "Iceberg Dark".to_string(),
    };
    assert_eq!(add_pack_into(&dir, "Iceberg Dark"), added);
    let file = dir.join("Iceberg Dark.conf");
    let text = aterm_theme_pack::find("Iceberg Dark").unwrap().text;
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    assert_eq!(add_pack_into(&dir, "iceberg dark"), added);
    assert!(matches!(
        add_pack_into(&dir, "Nope"),
        ThemesOutcome::Failed { .. }
    ));
    std::fs::write(&file, "mine").unwrap();
    assert!(matches!(
        add_pack_into(&dir, "Iceberg Dark"),
        ThemesOutcome::Failed { .. }
    ));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every pack theme, installed, loads through the catalog the window
/// reads, as exactly the colours its file holds. Negative control: a
/// broken copy beside them is listed, not loaded, with its reason.
#[test]
fn pack_themes_load_through_the_catalog() {
    let dir = scratch("pack-catalog");
    for theme in aterm_theme_pack::themes() {
        aterm_theme_pack::install(theme, Some(&dir), false).expect("installs");
    }
    std::fs::write(
        dir.join("Broken Copy.conf"),
        "foreground = nonsense
",
    )
    .unwrap();
    let catalog = crate::app_config::ThemeCatalog::discover_in(&dir);
    for theme in aterm_theme_pack::themes() {
        let parsed = aterm_types::scheme::parse_theme_str(theme.text).unwrap();
        assert_eq!(catalog.resolve(theme.id), Ok(parsed.scheme), "{}", theme.id);
        assert!(
            catalog.ready_names().any(|name| name == theme.id),
            "{}",
            theme.id
        );
    }
    let broken = catalog
        .listing()
        .find(|(name, _)| *name == "Broken Copy")
        .map(|(_, status)| status);
    assert!(
        matches!(
            broken,
            Some(crate::app_config::ThemeEntryStatus::Invalid(reason)) if !reason.is_empty()
        ),
        "{broken:?}"
    );
    assert!(catalog.resolve("Broken Copy").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
