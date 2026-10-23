// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The host half of Settings' theme picker footer and Needs-attention rows.

use crate::native_app::{ThemesOutcome, ThemesRequest};

/// What the themes host reaches beyond the themes folder.
#[derive(Clone, Copy)]
pub(crate) struct ThemesHostArms {
    /// Move a theme file to where it can be put back.
    trash: fn(&std::path::Path) -> Result<(), String>,
    /// Show a theme file or the folder in the platform's file manager.
    open: fn(&std::path::Path) -> Result<(), String>,
    /// Ask the person for a file to import: `Ok(None)` when they cancel.
    choose: fn() -> Result<Option<std::path::PathBuf>, String>,
}

impl ThemesHostArms {
    /// The isolated arms for a headless instance, the live ones otherwise.
    pub(crate) fn for_instance(headless: bool) -> Self {
        if headless {
            Self::isolated()
        } else {
            Self::live()
        }
    }

    fn live() -> Self {
        Self {
            trash: move_to_trash,
            open: open_in_workspace,
            choose: choose_import,
        }
    }

    fn isolated() -> Self {
        Self {
            trash: move_to_local_trash,
            open: refuse_open,
            choose: refuse_choose,
        }
    }
}

fn open_in_workspace(target: &std::path::Path) -> Result<(), String> {
    if crate::menu::open_file_in_workspace(target) {
        Ok(())
    } else {
        Err(format!("could not open {}", target.display()))
    }
}

fn choose_import() -> Result<Option<std::path::PathBuf>, String> {
    Ok(crate::menu::choose_local_file("Import a theme", "Import"))
}

fn move_to_local_trash(file: &std::path::Path) -> Result<(), String> {
    move_to_trash_fallback(file, |_| {})
}

fn refuse_open(_target: &std::path::Path) -> Result<(), String> {
    Err("a headless instance opens nothing on the desktop".to_string())
}

fn refuse_choose() -> Result<Option<std::path::PathBuf>, String> {
    Err("a headless instance shows no file picker".to_string())
}

/// Perform `request` against the live themes folder, reaching beyond it only through `arms`.
pub(crate) fn perform(request: &ThemesRequest, arms: ThemesHostArms) -> ThemesOutcome {
    let Some(dir) = aterm_types::scheme::user_theme_dir() else {
        return ThemesOutcome::Failed {
            message: "no themes folder: neither XDG_CONFIG_HOME, APPDATA nor HOME is set"
                .to_string(),
        };
    };
    perform_in(&dir, request, arms)
}

/// [`perform`] against the themes folder `dir`.
fn perform_in(
    dir: &std::path::Path,
    request: &ThemesRequest,
    arms: ThemesHostArms,
) -> ThemesOutcome {
    match request {
        ThemesRequest::OpenFolder => open(dir, dir, true, arms.open),
        ThemesRequest::OpenFile { id } => {
            let file = theme_file(dir, id);
            match file {
                Some(file) => open(&file, dir, false, arms.open),
                None => open(dir, dir, true, arms.open),
            }
        }
        ThemesRequest::Import => match (arms.choose)() {
            Ok(Some(path)) => import_from(&path, dir),
            Ok(None) => ThemesOutcome::Cancelled,
            Err(message) => ThemesOutcome::Failed { message },
        },
        ThemesRequest::Save { id, text } => save_into(dir, id, text),
        ThemesRequest::Trash { id } => trash_from(dir, id, arms.trash),
        ThemesRequest::AddPack { id } => add_pack_into(dir, id),
    }
}

/// Install the community pack theme `id` into `dir`, never over a different file of that name.
pub(crate) fn add_pack_into(dir: &std::path::Path, id: &str) -> ThemesOutcome {
    let Some(theme) = aterm_theme_pack::find(id) else {
        return ThemesOutcome::Failed {
            message: format!("{id} is not in the community pack"),
        };
    };
    match aterm_theme_pack::install(theme, Some(dir), false) {
        Ok(_) => ThemesOutcome::Added {
            id: theme.id.to_string(),
        },
        Err(aterm_types::scheme_install::InstallError::Exists(_)) => ThemesOutcome::Failed {
            message: format!(
                "a theme named \"{}\" is already in the themes folder",
                theme.id
            ),
        },
        Err(error) => ThemesOutcome::Failed {
            message: error.to_string(),
        },
    }
}

/// Move theme `id`'s file out of `dir` with `mover`.
pub(crate) fn trash_from(
    dir: &std::path::Path,
    id: &str,
    mover: fn(&std::path::Path) -> Result<(), String>,
) -> ThemesOutcome {
    if aterm_types::scheme::builtin_theme(id).is_some() {
        return ThemesOutcome::Failed {
            message: format!("{id} is a built-in theme"),
        };
    }
    let Some(file) = theme_file(dir, id) else {
        return ThemesOutcome::Failed {
            message: format!("{id} is not a file in the themes folder"),
        };
    };
    match mover(&file) {
        Ok(()) => ThemesOutcome::Trashed { id: id.to_string() },
        Err(why) => ThemesOutcome::Failed {
            message: format!("couldn\u{2019}t move {id} to the Trash: {why}"),
        },
    }
}

/// The live mover: the system's trash tool where it has one (macOS ships
/// it), else a `.trash` folder beside the themes, which the catalog does not
/// read. Either way the file can be put back.
fn move_to_trash(file: &std::path::Path) -> Result<(), String> {
    if crate::consent_retire::trash_tool_present() {
        return atpkg::copies::trash(file);
    }
    move_to_trash_fallback(file, |_| {})
}

fn move_to_trash_fallback(
    file: &std::path::Path,
    mut before_publish: impl FnMut(&std::path::Path),
) -> Result<(), String> {
    let (Some(dir), Some(name)) = (file.parent(), file.file_name()) else {
        return Err("the file has no folder".to_string());
    };
    let dir =
        crate::pinned_dir::PinnedDir::open_resolved(dir).map_err(|error| error.to_string())?;
    let source = dir
        .pin_private_file(name)
        .map_err(|error| error.to_string())?;
    let bin_name = std::ffi::OsStr::new(".trash");
    let bin = match dir.create_child(bin_name) {
        Ok(bin) => bin,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // `child` opens without following a link or reparse point.
            dir.child(bin_name).map_err(|error| error.to_string())?
        }
        Err(error) => return Err(error.to_string()),
    };
    for _ in 0..5 {
        let token = aterm_uds::rand::hex_token::<16>().map_err(|error| error.to_string())?;
        let recovery_name = format!("theme-{token}");
        let dest = bin.path().join(&recovery_name);
        before_publish(&dest);
        let recovery = match bin.create_child(std::ffi::OsStr::new(&recovery_name)) {
            Ok(recovery) => recovery,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        };
        return source.move_to_recovery(&recovery, name).map_err(|error| {
            format!(
                "recovery folder {} retained: {error}",
                recovery.path().display()
            )
        });
    }
    Err("could not claim a new recovery directory; the theme was not moved".to_string())
}

/// Write `text` into `dir` as the new theme `id`.
pub(crate) fn save_into(dir: &std::path::Path, id: &str, text: &str) -> ThemesOutcome {
    if let Err(why) = aterm_types::scheme::validate_user_theme_name(id) {
        return ThemesOutcome::Failed {
            message: format!("{id:?} cannot name a theme: {why}"),
        };
    }
    if aterm_types::scheme::builtin_theme(id).is_some() {
        return ThemesOutcome::Failed {
            message: format!("{id:?} is a built-in theme's name"),
        };
    }
    if text.len() > aterm_types::scheme::MAX_USER_THEME_FILE_BYTES {
        return ThemesOutcome::Failed {
            message: "the theme is larger than a theme file may be".to_string(),
        };
    }
    if let Err(error) = aterm_types::scheme::parse_theme_str(text) {
        return ThemesOutcome::Failed {
            message: format!("the theme did not read back: {error}"),
        };
    }
    let dest = dir.join(format!("{id}.conf"));
    match aterm_types::scheme_install::write_atomically(dir, &dest, id, text.as_bytes(), false) {
        Ok(()) => ThemesOutcome::Saved { id: id.to_string() },
        Err(aterm_types::scheme_install::InstallError::Exists(_)) => ThemesOutcome::Failed {
            message: format!("a theme named {id:?} is already in the themes folder"),
        },
        Err(error) => ThemesOutcome::Failed {
            message: error.to_string(),
        },
    }
}

/// The file behind catalog id `id` in `dir`, when it is a regular file there.
/// An id that is not a valid theme name (an unreadable row's lossy spelling)
/// names no file.
fn theme_file(dir: &std::path::Path, id: &str) -> Option<std::path::PathBuf> {
    aterm_types::scheme::validate_user_theme_name(id).ok()?;
    let file = dir.join(format!("{id}.conf"));
    std::fs::symlink_metadata(&file)
        .is_ok_and(|meta| meta.is_file())
        .then_some(file)
}

/// Import `path` into `dir` as a new theme.
pub(crate) fn import_from(path: &std::path::Path, dir: &std::path::Path) -> ThemesOutcome {
    match aterm_types::scheme_install::install_theme_file_as_new(path, Some(dir)) {
        Ok(installed) => ThemesOutcome::Imported {
            id: installed.id,
            dropped: installed.dropped.len(),
        },
        Err(aterm_types::scheme_install::InstallError::Exists(dest)) => ThemesOutcome::Failed {
            message: format!(
                "a theme named {:?} is already in the themes folder",
                dest.file_stem().map_or_else(
                    || dest.display().to_string(),
                    |stem| stem.to_string_lossy().into_owned()
                )
            ),
        },
        Err(error) => ThemesOutcome::Failed {
            message: error.to_string(),
        },
    }
}

fn open(
    target: &std::path::Path,
    dir: &std::path::Path,
    create: bool,
    opener: fn(&std::path::Path) -> Result<(), String>,
) -> ThemesOutcome {
    if create && let Err(error) = std::fs::create_dir_all(dir) {
        return ThemesOutcome::Failed {
            message: format!("could not create the themes folder: {error}"),
        };
    }
    match opener(target) {
        Ok(()) => ThemesOutcome::Opened,
        Err(message) => {
            aterm_log::warn!("{message}");
            ThemesOutcome::Failed { message }
        }
    }
}

impl crate::App {
    /// Read the themes folder again off the event loop and hand the catalog to the ordinary.
    pub(crate) fn rediscover_themes_now(&self) {
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let Some(dir) = aterm_types::scheme::user_theme_dir() else {
            return;
        };
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-theme-rescan".to_string(),
            crate::qos::Role::Background,
            move || {
                if let Ok(catalog) = crate::app_config::ThemeCatalog::try_discover_in(&dir) {
                    let _ = proxy.send_event(crate::Wake::ThemeCatalogChanged(
                        std::sync::Arc::new(catalog),
                    ));
                }
            },
        );
        if let Err(error) = spawned {
            aterm_log::warn!("could not start the theme folder rescan: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
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
}
#[cfg(test)]
mod fallback_tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn fallback_trash_does_not_follow_a_link_to_a_foreign_folder() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("theme-trash-link-")
            .tempdir()
            .unwrap();
        let root = scratch.path();
        let themes = root.join("themes");
        let foreign = root.join("foreign");
        std::fs::create_dir_all(&themes).unwrap();
        std::fs::create_dir_all(&foreign).unwrap();
        let file = themes.join("mine.conf");
        std::fs::write(&file, "theme to retain").unwrap();
        std::os::unix::fs::symlink(&foreign, themes.join(".trash")).unwrap();
        assert!(move_to_trash_fallback(&file, |_| {}).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "theme to retain");
        assert_eq!(std::fs::read_dir(&foreign).unwrap().count(), 0);
    }

    #[test]
    fn fallback_trash_keeps_a_recovery_file_that_wins_publication() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("theme-trash-collision-")
            .tempdir()
            .unwrap();
        let root = scratch.path();
        let file = root.join("mine.conf");
        std::fs::write(&file, "new theme").unwrap();
        let mut raced = false;
        let mut winner = None;
        move_to_trash_fallback(&file, |dest| {
            if !raced {
                std::fs::write(dest, "earlier recoverable theme").unwrap();
                winner = Some(dest.to_path_buf());
                raced = true;
            }
        })
        .unwrap();
        assert!(raced);
        assert_eq!(
            std::fs::read_to_string(winner.unwrap()).unwrap(),
            "earlier recoverable theme"
        );
        assert!(!file.exists());
        let mut paths = vec![root.join(".trash")];
        let mut recovered = Vec::new();
        while let Some(path) = paths.pop() {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    paths.push(path);
                } else {
                    recovered.push(std::fs::read_to_string(path).unwrap());
                }
            }
        }
        recovered.sort();
        assert_eq!(recovered, ["earlier recoverable theme", "new theme"]);
    }

    #[test]
    fn fallback_trash_does_not_discard_a_replaced_source() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("theme-trash-replaced-")
            .tempdir()
            .unwrap();
        let root = scratch.path();
        let file = root.join("mine.conf");
        let earlier = root.join("earlier.conf");
        std::fs::write(&file, "original theme").unwrap();
        let result = move_to_trash_fallback(&file, |_| {
            std::fs::rename(&file, &earlier).unwrap();
            std::fs::write(&file, "new unrelated theme").unwrap();
        });
        assert!(
            result.is_err(),
            "a replaced source is not the admitted theme"
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "new unrelated theme"
        );
        assert_eq!(std::fs::read_to_string(&earlier).unwrap(), "original theme");
    }
}
