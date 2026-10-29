// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The file watch over open documents (A4, `docs/DESIGN-amacs-2026-10-19.md`
//! §7): every document opened from a file is watched
//! ([`amacs_files::watch`]), and when its file changes on disk, whoever
//! changed it (an AI CLI's edit tool, `git checkout`, a formatter), the host
//! re-reads it through the document's own capability and reduces it
//! ([`App::refresh_native_document`]).
//!
//! A clean buffer reloads in place as one labelled disk undo frame, its
//! caret, mark and scroll kept on their text. Over unsaved edits the editor's
//! header row offers Reload (the disk's text as an undo frame, so C-/ brings
//! the edits back) or Keep Mine (the buffer stays as it is, unsaved, and the
//! file as it is now becomes what Save writes over). A file deleted under its
//! buffer keeps the buffer's text and offers Keep Mine, after which Save
//! writes the file again. Neither side is lost: the file keeps the disk's
//! text until the person saves.
//!
//! The running app starts the platform watch at its first watched document,
//! and the watch thread wakes the event loop
//! ([`crate::Wake::NativeDocumentFileChanged`]). The headless app has no
//! event loop and starts no watch: it records what it would watch, and a test
//! delivers the wake by hand.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::DiskConflictCause;
use crate::App;
use crate::document_store::DocumentId;
use crate::native_app::{AppViewState, DiskNotice};
use crate::native_document_host::DEFAULT_DOCUMENT_LIMIT;

/// What the footer says once Keep Mine has kept the buffer's text.
pub(crate) const KEPT_STATUS: &str = "Kept your edits · Save writes them over the file on disk";

/// The documents the file watch hears, and the watch once it started.
#[derive(Default)]
pub(crate) struct DocumentWatch {
    watch: Option<amacs_files::watch::FileWatch>,
    /// The platform refused the watch's event source: logged once, not retried.
    unavailable: bool,
    /// Each watched key (its document's id) with the document and file.
    watched: BTreeMap<u64, (DocumentId, PathBuf)>,
    /// Documents whose file changed while their own save was in flight,
    /// looked at again once the save lands.
    owed: BTreeSet<DocumentId>,
}

impl DocumentWatch {
    fn document(&self, key: u64) -> Option<DocumentId> {
        self.watched.get(&key).map(|(document, _)| *document)
    }

    fn path(&self, document: DocumentId) -> Option<&Path> {
        self.watched
            .get(&document.get())
            .map(|(_, path)| path.as_path())
    }

    /// Stop watching `document` (it was released).
    pub(super) fn unwatch(&mut self, document: DocumentId) {
        let key = document.get();
        self.owed.remove(&document);
        if self.watched.remove(&key).is_some()
            && let Some(watch) = &self.watch
        {
            watch.unwatch(key);
        }
    }

    /// The file `document` is watched at (a test reads what the headless app
    /// would watch).
    #[cfg(test)]
    pub(crate) fn watched_path(&self, document: DocumentId) -> Option<&Path> {
        self.path(document)
    }
}

impl App {
    /// Watch `document`'s file, from its open on (again after the grant moved).
    pub(super) fn watch_document_file(&mut self, document: DocumentId) {
        let Some(path) = self
            .native_documents
            .persistence
            .grant(document)
            .and_then(|grant| self.native_documents.grants.get(grant))
            .map(|grant| grant.target().target_path().to_path_buf())
        else {
            return;
        };
        let key = document.get();
        let host = &mut self.native_documents.watch;
        if host
            .watched
            .get(&key)
            .is_some_and(|(_, watched)| *watched == path)
        {
            return;
        }
        host.watched.insert(key, (document, path.clone()));
        if let Some(watch) = &host.watch {
            watch.watch(key, path);
            return;
        }
        // The headless app has no event loop to wake, so it starts no watch.
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        if host.unavailable {
            return;
        }
        let notify = move |key: u64| {
            let _ = proxy.send_event(crate::Wake::NativeDocumentFileChanged(key));
        };
        match amacs_files::watch::FileWatch::platform(Box::new(notify)) {
            Ok(watch) => {
                for (key, (_, path)) in &host.watched {
                    watch.watch(*key, path.clone());
                }
                host.watch = Some(watch);
            }
            Err(error) => {
                host.unavailable = true;
                aterm_log::warn!("document file watch unavailable: {error}");
            }
        }
    }

    /// The watch heard `key`'s file change: re-read it and reduce (a clean
    /// buffer reloads in place, a dirty one gets the header row's notice).
    pub(crate) fn native_document_file_changed(&mut self, key: u64) {
        let Some(document) = self.native_documents.watch.document(key) else {
            return;
        };
        // Settings ▸ Manual's aterm.toml has its own watcher, which checks
        // the file's binding as well.
        if self.native_runtime.config_editor_document() == Some(document) {
            return;
        }
        if self.native_documents.persistence.save_in_flight(document) {
            self.native_documents.watch.owed.insert(document);
            return;
        }
        match self.refresh_native_document(document) {
            Ok(crate::native_document_io::FileWatchReduction::DeferredSaving { .. }) => {
                self.native_documents.watch.owed.insert(document);
            }
            Ok(_) => {}
            Err(_) if self.document_file_gone(document) => {
                self.set_document_disk_conflict(document, Some(DiskConflictCause::Version));
                self.set_document_recovery_status(
                    document,
                    Some(crate::native_app::DISK_EXISTENCE_RECOVERY.to_string()),
                );
                self.note_disk_change(document);
            }
            Err(error) => {
                aterm_log::warn!("document file watch: {error}");
                for (_, _, view) in self.document_native_views(document) {
                    self.set_editor_view_status(view, &error);
                }
            }
        }
    }

    /// A save landed: look again at a change the watch heard while it was
    /// in flight.
    pub(super) fn look_again_after_save(&mut self, document: DocumentId) {
        if self.native_documents.watch.owed.remove(&document) {
            self.native_document_file_changed(document.get());
        }
    }

    /// Whether `document`'s file is gone from disk.
    fn document_file_gone(&self, document: DocumentId) -> bool {
        self.native_documents
            .watch
            .path(document)
            .is_some_and(|path| std::fs::symlink_metadata(path).is_err())
    }

    /// The file changed (or went) under a buffer with a version conflict:
    /// the header row says which and offers the choice.
    pub(super) fn note_disk_change(&mut self, document: DocumentId) {
        let notice = if self.document_file_gone(document) {
            DiskNotice::Gone
        } else {
            DiskNotice::Changed
        };
        if self
            .native_runtime
            .set_document_disk_notice(document, Some(notice))
        {
            self.repaint_document_views(document);
        }
    }

    /// Keep Mine: the file as it is on disk now becomes the version Save
    /// writes over, and the buffer stays exactly as it is (its text, undo
    /// history, carets, scroll, unsaved state and draft journal), so Save
    /// writes the person's text over the file knowingly. One step: nothing
    /// changes unless the file is read and rebound.
    ///
    /// # Errors
    /// When the file did not change under the buffer, a save is in flight,
    /// or the file cannot be read again or rebound.
    pub(super) fn keep_document_over_disk(&mut self, document: DocumentId) -> Result<(), String> {
        if self.native_documents.disk_conflicts.get(&document) != Some(&DiskConflictCause::Version)
        {
            return Err(
                "Nothing to keep: the file on disk did not change under this buffer".into(),
            );
        }
        if self.native_documents.persistence.save_in_flight(document) {
            return Err("cannot keep while a save is in flight".to_string());
        }
        let grant = self
            .native_documents
            .persistence
            .grant(document)
            .ok_or_else(|| "document persistence grant disappeared".to_string())?;
        // A deleted file reads as missing: Save then writes it again.
        let observed = self
            .native_documents
            .grants
            .refresh_local_allowing_missing(grant, DEFAULT_DOCUMENT_LIMIT)
            .map_err(|error| format!("could not read the file on disk: {error}"))?;
        self.native_documents
            .persistence
            .accept_overwrite(document, observed.observed)
            .map_err(|error| format!("could not keep your edits: {error:?}"))?;
        for (_, _, view) in self.document_native_views(document) {
            if let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(view) {
                state.status = Some(KEPT_STATUS.to_string());
            }
        }
        self.set_document_recovery_status(document, None);
        self.set_document_disk_conflict(document, None);
        self.repaint_document_views(document);
        Ok(())
    }
}
