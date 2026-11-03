// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Write the pinned default ALab vault's `open = yes` line as its open note
//! ([`crate::text::OPEN_NOTE`]), which aterm 0.147 and earlier read past as a comment. Their
//! approvers then read the vault as closed, and stop letting in a request their own server took
//! for a LAN one by its address alone (a phone hotspot's, a café's); aterm 0.148 and later read
//! it open as before. Only an ALab admin's ordinary approver pass publishes, once for each
//! version that still holds the line (an earlier aterm's `password open` writes it again).

use super::*;

/// How long after a failed step the next waits, so a publish that keeps failing does not take
/// every approver pass.
const AFTER_FAILURE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) struct Rounds {
    /// The vault version last read that held no `open = yes` line.
    checked: Option<u64>,
    failed_at: Option<Instant>,
}

impl Rounds {
    /// Publish the ALab vault at `config` (version `seq`) with its `open = yes` line written as
    /// the open note, where it holds one. True only after a publication; the caller reads the
    /// new admin version at once.
    pub(super) fn step(
        &mut self,
        paths: &Paths,
        config: &Config,
        seq: u64,
        log: &dyn Fn(&str),
    ) -> bool {
        if config.name != crate::ALAB_VAULT
            || !alab_vault(config)
            || crate::service::subscriber_turned_off(&paths.home)
        {
            self.checked = None;
            return false;
        }
        if self.checked == Some(seq)
            || self
                .failed_at
                .is_some_and(|at| at.elapsed() < AFTER_FAILURE)
        {
            return false;
        }
        match self.try_step(paths, config, seq, log) {
            Ok(published) => {
                self.failed_at = None;
                published
            }
            Err(error) => {
                // Admitting members goes on whatever this step meets.
                self.failed_at = Some(Instant::now());
                log(&format!("the vault's open line: {error}"));
                false
            }
        }
    }

    fn try_step(
        &mut self,
        paths: &Paths,
        config: &Config,
        seq: u64,
        log: &dyn Fn(&str),
    ) -> crate::Result<bool> {
        let admin = admin_open(paths)?;
        if admin.config != *config || admin.seq != seq {
            return Err(Error::new("the vault changed while reading its open line"));
        }
        let Some(doc) = quieted(&admin.published_doc)? else {
            self.checked = Some(seq);
            return Ok(false);
        };
        admin_publish(paths, &admin, &doc)?;
        self.checked = None;
        log(
            "wrote the vault's `open = yes` line as its open note, which aterm 0.147 and earlier read as closed",
        );
        Ok(true)
    }
}

/// `doc` with its `open = yes` line written as the open note, where it holds one.
fn quieted(doc: &Document) -> crate::Result<Option<Document>> {
    if !doc.open_line() {
        return Ok(None);
    }
    let mut doc = doc.clone();
    doc.set_open(true)
        .map_err(|error| Error::new(error.to_string()))?;
    Ok(Some(doc))
}

#[cfg(test)]
#[path = "open_migration_tests.rs"]
mod tests;
