// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The update record's glue to the lab: the updater (`aterm-update`) and the lab model
//! (`aterm-db`) do not link each other, so this process joins them. The journal's flusher gets
//! this copy's update record (`aterm_db::alab::journal::upd::install_record`), read from the
//! updater's own file each pass, and publishes it as this Mac's `upd` row; an edit that sets or
//! ends a hold or notes a trial miss nudges it (`update_record::install_notify`); and the updater
//! reads every lab Mac's live row through `aterm_update::lab::install_lab`, the evidence that
//! cuts a hold short once when another Mac runs the same artifact proven, and what
//! `aterm update holds` lists. Installed in every process at its start; a process that holds no
//! lab (a debug build, a test, a scratch state root, CI: `alab_view::holds`) installs nothing,
//! so its record stays its own and its holds end only by their schedule, a newer build or a
//! person.

use aterm_db::alab::journal::upd;
use aterm_secrets::client::Paths;
use aterm_update::lab::{LabRow, LabUpdates};
use aterm_update_core::update_record::{self, UpdateRecord};
use std::path::PathBuf;

/// The lab's `upd` rows, read from this Mac's Commons replica.
struct Lab {
    paths: Paths,
}

impl LabUpdates for Lab {
    fn rows(&self) -> Option<Vec<LabRow>> {
        let now = aterm_time::unix_secs()?;
        match upd::read(&self.paths, now) {
            Ok(rows) => Some(
                rows?
                    .into_iter()
                    .map(|row| LabRow {
                        machine: row.uuid,
                        record: row.record,
                        own: row.own,
                    })
                    .collect(),
            ),
            Err(why) => {
                aterm_log::debug!("lab update records: {why}");
                None
            }
        }
    }
}

/// This copy's update record as its file holds it now; `None` with no file or one that does not
/// read, which leaves the published row as it was.
fn own_record() -> Option<UpdateRecord> {
    UpdateRecord::read(&aterm_update::record_path()?)
        .ok()
        .flatten()
}

/// Join the updater's record to the lab for this process (see the module doc).
pub(crate) fn install() {
    if !crate::alab_view::holds() {
        return;
    }
    let Some(home) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
    else {
        return;
    };
    let _ = upd::install_record(Box::new(own_record));
    let _ = update_record::install_notify(Box::new(upd::nudge));
    let _ = aterm_update::lab::install_lab(Box::new(Lab {
        paths: Paths::for_home(&home),
    }));
}
