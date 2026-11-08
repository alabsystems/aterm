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
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// `[swarm] enabled = false` starts no Commons runtime, and a person turning it on starts
    /// one; with the switch on, it starts at once.
    #[test]
    fn swarm_off_starts_no_commons_runtime_until_it_is_on() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path();
        let config =
            aterm_types::dirs::resolve_aterm_config_path(None, None, Some(home.as_os_str()))
                .unwrap();
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        aterm_uds::atomic_file::write(&config, b"[swarm]\nenabled = false\n", 0o600).unwrap();
        for enabled in [false, true] {
            std::fs::write(&config, format!("[swarm]\nenabled = {enabled}\n")).unwrap();
            let attempts = Arc::new(AtomicUsize::new(0));
            let tried = Arc::clone(&attempts);
            let mut startup =
                super::Startup::new(home, move || tried.fetch_add(1, Ordering::SeqCst) > 0);
            let now = std::time::Instant::now();
            let due = startup.maintain_at(now).unwrap();
            assert_eq!(attempts.load(Ordering::SeqCst), usize::from(enabled));
            let due = if !enabled {
                assert_eq!(due, now + super::SHARING_LOOK);
                std::fs::write(&config, "[swarm]\nenabled = true\n").unwrap();
                startup.maintain_at(due).unwrap()
            } else {
                due
            };
            assert_eq!(attempts.load(Ordering::SeqCst), 1);
            startup.maintain_at(due - std::time::Duration::from_nanos(1));
            assert_eq!(attempts.load(Ordering::SeqCst), 1);
            // A failed constructor never gives a later retry permission to bypass sharing.
            std::fs::write(&config, "[swarm]\nenabled = false\n").unwrap();
            let held = startup.maintain_at(due).unwrap();
            assert_eq!(attempts.load(Ordering::SeqCst), 1);
            std::fs::write(&config, "[swarm]\nenabled = true\n").unwrap();
            assert_eq!(startup.maintain_at(held), None);
            assert_eq!(attempts.load(Ordering::SeqCst), 2);
            assert_eq!(startup.maintain_at(held + super::SHARING_LOOK), None);
            assert_eq!(attempts.load(Ordering::SeqCst), 2);
        }
    }
}
