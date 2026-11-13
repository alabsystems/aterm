// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OWNER'S KEYS IN A SAVE OF aterm.toml (`docs/DESIGN-harness-occupancy-2026-10-20.md` §1.1,
//! §3.3): root `[harness]`'s `occupancy`, `occupancy_source` and `occupancy_rule`
//! ([`aterm_agent::supervise::config::OWNER_KEYS`]) change only by a person at the window.
//!
//! A control client works an Editor buffer with the window's rights: the `edit` verb (`amacs
//! --no-wait`) opens a file in one, `act` presses Settings ▸ Harness's Open aterm.toml, the
//! `key`, `text` and `paste` verbs type into the front native view
//! ([`crate::App::input_to_session`]), and a key, `act` or `invoke Save` saves it. A key, click or
//! scroll another process posts into the window (System Events, `CGEventPost` from a program in a
//! tab) is no person's here either ([`Unvouched::Relayed`], [`crate::hwkey::classify`]). An Editor
//! also saves its document when its tab closes or the app quits, whoever closed it. So a save of
//! the aterm.toml in force whose owner keys differ from the file's is refused when a control
//! client or another process's input made the save, or when an edit no person here is known to
//! have made reached the buffer ([`Unvouched`]); a save queued behind one in flight is replayed
//! under the mark of whoever asked for it ([`queued_by_controller`]). An accessibility client's
//! edit or press counts as a control client's here ([`Unvouched::Assistive`]): every tab's
//! program reaches the window's accessibility tree with aterm's own grant, so aterm cannot tell
//! its request from a screen reader's. While the aterm.toml in force names no file, a save that
//! makes one in the folder it would be made in counts as a save of it, whatever the new file's
//! name ([`is_in_force`]). Discarding the mark's buffer starts its undo at the file's text
//! ([`discard_starts_history`]). Every other save goes on as before.

use std::path::{Path, PathBuf};

use aterm_agent::supervise::config::OwnerKeys;

/// An edit in a document that no person at this window is known to have made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unvouched {
    /// A control client's gesture made it.
    Controller,
    /// An accessibility client's request made it (`setAccessibilitySelectedText:`, a press
    /// through the window's accessibility tree): a person's assistive technology and a program
    /// holding aterm's Accessibility grant, as every tab's program does through aterm, send the
    /// same request ([`crate::App::with_assistive_source`]).
    Assistive,
    /// A key, click or scroll another process posted into the window made it
    /// ([`crate::hwkey::NativeInput`]'s source is neither the HID system nor aterm): a person
    /// relaying their own input (Screen Sharing, an on-screen keyboard) and a program holding
    /// aterm's Accessibility grant, as every tab's program does through aterm, post the same
    /// events ([`crate::App::with_origin_mark`]).
    Relayed,
    /// A journal's draft put it there when the document opened: the process that wrote the
    /// journal knew who typed it, this one does not.
    Recovered,
}

/// Who stands behind a save's text: the save's own gesture, and the buffer's edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Provenance {
    controller_save: bool,
    edit: Option<Unvouched>,
}

impl Provenance {
    /// A save a control client, an accessibility client or another process's input made
    /// (`controller_save`) of a buffer `edit` reached.
    pub(crate) fn new(controller_save: bool, edit: Option<Unvouched>) -> Self {
        Self {
            controller_save: controller_save && !fault::armed(fault::Fault::SaveIgnoresSource),
            edit: edit.filter(|_| !fault::armed(fault::Fault::SaveIgnoresEdits)),
        }
    }

    /// Whether a save of the aterm.toml in force must keep the file's owner keys: when anyone
    /// but a person at the window stands behind its text.
    pub(crate) const fn fenced(self) -> bool {
        self.controller_save || self.edit.is_some()
    }

    /// Why a save of the aterm.toml in force is refused: `saved` is the text it would write, and
    /// `disk` the file's as the window reads it (empty where there is none) or `Err` where it
    /// could not be read. `None` where the owner keys read the same, or nothing is fenced.
    pub(crate) fn refusal(self, saved: &str, disk: Result<&str, ()>) -> Option<String> {
        if !self.fenced() {
            return None;
        }
        let Ok(disk) = disk else {
            return Some(
                "Save blocked · aterm.toml's [harness] occupancy keys are the owner's, and the \
                 file on disk could not be read to compare them"
                    .to_string(),
            );
        };
        if OwnerKeys::of(saved) == OwnerKeys::of(disk) {
            return None;
        }
        Some(match (self.controller_save, self.edit) {
            (true, _) => "Save blocked · aterm.toml's [harness] occupancy keys are the owner's; \
                          edit them in the window yourself"
                .to_string(),
            (false, Some(Unvouched::Assistive)) => {
                "Save blocked · aterm.toml's [harness] occupancy keys are the owner's, and an \
                 accessibility client edited this buffer; choose ‘Discard Changes and Reload \
                 from Disk’, then edit them yourself"
                    .to_string()
            }
            (false, Some(Unvouched::Relayed)) => {
                "Save blocked · aterm.toml's [harness] occupancy keys are the owner's, and \
                 another program's keys or clicks edited this buffer; choose ‘Discard Changes \
                 and Reload from Disk’, then edit them yourself"
                    .to_string()
            }
            (false, Some(Unvouched::Recovered)) => {
                "Save blocked · aterm.toml's [harness] occupancy keys are the owner's, and this \
                 buffer is a recovered draft; choose ‘Discard Changes and Reload from Disk’, \
                 then edit them yourself"
                    .to_string()
            }
            (false, _) => "Save blocked · aterm.toml's [harness] occupancy keys are the owner's, \
                           and a control client edited this buffer; choose ‘Discard Changes and \
                           Reload from Disk’, then edit them yourself"
                .to_string(),
        })
    }
}

/// Whether a save queued behind one in flight is a control client's, an accessibility client's
/// or another process's input's (`controller`): its replay, when the one in flight lands, is made
/// under a control client's mark.
pub(crate) fn queued_by_controller(controller: bool) -> bool {
    controller && !fault::armed(fault::Fault::QueueDropsSource)
}

/// Whether "Discard Changes and Reload from Disk" of a buffer that held an [`Unvouched`] edit
/// starts its undo at the file's text: it does, so no person's `C-/` or redo puts that edit back
/// once the mark is gone.
pub(crate) fn discard_starts_history() -> bool {
    !fault::armed(fault::Fault::DiscardKeepsHistory)
}

/// The aterm.toml in force: the config service's `bound` path, else (its load failed at launch)
/// the path the window loads by, [`crate::app_config::config_path`]. A test's fallback is the
/// scratch path it names (`unbound::with`) or none, so no test looks at the owner's file.
pub(crate) fn in_force(bound: Option<&Path>) -> Option<PathBuf> {
    bound.map(Path::to_path_buf).or_else(unbound::path)
}

/// Whether `grant`'s file is the aterm.toml in force at `in_force`: the same path, spelled or
/// reached through admitted links; the same file (device and inode) under another name; or,
/// while no file is there, a file the save would make in the folder a write of `in_force`
/// would make it in ([`made_where_in_force_would_be`]).
pub(crate) fn is_in_force(
    grant: &crate::native_document_host::DocumentGrant,
    in_force: &Path,
) -> bool {
    let target = grant.target().target_path();
    (!fault::armed(fault::Fault::PathIgnored) && grant.targets_logical_path(in_force))
        || same_file(in_force, target)
        || (!fault::armed(fault::Fault::CreateIgnoresFolder)
            && made_where_in_force_would_be(in_force, target))
}

/// The most links [`folder_in_force_would_be_made_in`] follows, as the system's `SYMLOOP_MAX`.
const MAX_LINKS: usize = 32;

/// Whether a save to `target`, which names no file, makes it in the folder a write of the
/// missing aterm.toml in force would make it in. Whatever its name: a volume that folds case or
/// Unicode normalization (macOS's default) reads `Aterm.toml` there as `aterm.toml`, so no
/// spelling of the name is told apart from it.
fn made_where_in_force_would_be(in_force: &Path, target: &Path) -> bool {
    let Some(folder) = folder_in_force_would_be_made_in(in_force) else {
        return false;
    };
    let made = matches!(
        std::fs::symlink_metadata(target),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    );
    made && target
        .parent()
        .is_some_and(|parent| same_file(&folder, parent))
}

/// The folder a write of `in_force` would make it in while it names no file: its own, or the
/// folder of the missing file its last link names (a dotfile manager's link to a file not made
/// yet). `None` once a file is there, or where `in_force` cannot be looked at, or its links do
/// not end.
fn folder_in_force_would_be_made_in(in_force: &Path) -> Option<PathBuf> {
    let mut path = in_force.to_path_buf();
    for _ in 0..MAX_LINKS {
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let destination = std::fs::read_link(&path).ok()?;
                path = path.parent()?.join(destination);
            }
            Ok(_) => return None,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return path.parent().map(Path::to_path_buf);
            }
            Err(_) => return None,
        }
    }
    None
}

/// Whether `a` and `b` name one existing file or folder.
#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Whether `a` and `b` name one existing file or folder: no device and inode here, so their
/// canonical spellings, which the system gives in each name's own case.
#[cfg(not(unix))]
fn same_file(a: &Path, b: &Path) -> bool {
    matches!(
        (std::fs::canonicalize(a), std::fs::canonicalize(b)),
        (Ok(a), Ok(b)) if a == b
    )
}

/// THE NEGATIVE CONTROLS' FAULT POINTS.
#[cfg(test)]
#[path = "owner_keys_save_fault_tests.rs"]
pub(crate) mod fault;

#[cfg(not(test))]
pub(crate) mod fault {
    /// A planted defect: none is armed outside the tests.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Fault {
        SaveIgnoresSource,
        SaveIgnoresEdits,
        QueueDropsSource,
        CreateIgnoresFolder,
        PathIgnored,
        DiscardKeepsHistory,
    }

    /// Never armed.
    pub(crate) const fn armed(_: Fault) -> bool {
        false
    }
}

/// The aterm.toml in force where the config service bound none: a test's own scratch path.
#[cfg(test)]
#[path = "owner_keys_save_unbound_tests.rs"]
pub(crate) mod unbound;

/// The aterm.toml in force where the config service bound none: the path the window loads by.
#[cfg(not(test))]
mod unbound {
    pub(super) fn path() -> Option<std::path::PathBuf> {
        crate::app_config::config_path()
    }
}
