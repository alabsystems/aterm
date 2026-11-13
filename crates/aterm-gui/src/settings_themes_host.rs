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
    /// Read the themes folder again off the event loop and hand the catalog
    /// to the ordinary catalog reload ([`crate::Wake::ThemeCatalogChanged`]),
    /// as the folder watcher does.
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
#[path = "settings_themes_host_fallback_tests.rs"]
mod fallback_tests;
#[cfg(test)]
#[path = "settings_themes_host_tests.rs"]
mod tests;
