// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! When this process's Commons runtime (a window's, a terminal session lane's, `aterm node
//! serve`'s) and its other automatic LAN uses may run: only while `[swarm] enabled` leaves
//! sharing on.

use std::path::Path;
use std::time::Instant;

/// How often a process whose Commons runtime `[swarm] enabled = false` holds looks at the switch
/// again: one `stat` ([`aterm_secrets::swarm::Switch`]).
pub(crate) const SHARING_LOOK: std::time::Duration = std::time::Duration::from_secs(5);

/// The process host retains a Commons start until the canonical runtime was published.
/// Its existing event loop supplies the deadline; recovery never needs another thread.
pub(crate) struct Startup {
    switch: aterm_secrets::swarm::Switch,
    constructor: crate::host_startup::Constructor,
    due: Option<Instant>,
}

impl Startup {
    pub(crate) fn new(home: &Path, start: impl FnMut() -> bool + Send + 'static) -> Self {
        Self {
            switch: aterm_secrets::swarm::Switch::new(home),
            constructor: crate::host_startup::Constructor::new(start),
            due: None,
        }
    }

    pub(crate) fn maintain(&mut self) -> Option<Instant> {
        if !self.constructor.pending() {
            return None;
        }
        self.maintain_at(Instant::now())
    }

    fn maintain_at(&mut self, now: Instant) -> Option<Instant> {
        if !self.constructor.pending() {
            return None;
        }
        if let Some(due) = self.due.or(self.constructor.deadline())
            && now < due
        {
            return Some(due);
        }
        // Unread sharing remains held, just as the runtime's own policy read does.
        if self
            .switch
            .enabled_saying(&|line| aterm_log::warn!("Commons: {line}"))
            != Ok(true)
        {
            self.due = Some(now + SHARING_LOOK);
            return self.due;
        }
        self.due = None;
        self.constructor.maintain_at(now)
    }
}

/// Install this process's reader of `home`'s `[swarm] enabled` for its automatic LAN uses
/// ([`aterm_types::macos_setup::sharing::install`]): while it reads off, or the file cannot be
/// read (said in aterm.log once per change), Tab Control's sweep does not go by itself
/// (`docs/DESIGN-alab-swarm-2026-10-27.md` §9). One `stat` a read
/// ([`aterm_secrets::swarm::Switch`]).
pub(crate) fn install_sharing(home: &Path) {
    let switch = std::sync::Mutex::new(aterm_secrets::swarm::Switch::new(home));
    aterm_types::macos_setup::sharing::install(move || {
        switch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .enabled_saying(&|line| aterm_log::warn!("automatic LAN uses: {line}"))
            == Ok(true)
    });
}

#[cfg(test)]
#[path = "commons_lan_tests.rs"]
mod tests;
