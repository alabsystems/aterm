// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! When this process's Commons runtime (a window's, a terminal session lane's, `aterm node
//! serve`'s) and its other automatic LAN uses may run: only while `[swarm] enabled` leaves
//! sharing on.

use std::path::Path;

/// How often a process whose Commons runtime `[swarm] enabled = false` holds looks at the switch
/// again ([`when_sharing_on`]): one `stat` ([`aterm_secrets::swarm::Switch`]).
pub(crate) const SHARING_LOOK: std::time::Duration = std::time::Duration::from_secs(5);

/// Run `start` (this process's Commons runtime) once `home`'s `[swarm] enabled` leaves sharing on:
/// at once where it does now, else on a thread of its own that looks at the switch each `look`
/// until a person turns it on. `docs/DESIGN-alab-swarm-2026-10-27.md` §9: the switch sets
/// `[commons] enabled = false` among the rest, so no Commons runtime runs while it is off. A switch that cannot be read holds it as an off one does, as Commons' own
/// policy read refuses it. Whether `start` ran at once.
pub(crate) fn when_sharing_on(
    home: &Path,
    look: std::time::Duration,
    start: impl FnOnce() + Send + 'static,
) -> bool {
    let mut switch = aterm_secrets::swarm::Switch::new(home);
    let said = |line: &str| aterm_log::warn!("Commons: {line}");
    if switch.enabled_saying(&said) == Ok(true) {
        start();
        return true;
    }
    let waits = std::thread::Builder::new()
        .name("commons-sharing-off".into())
        .spawn(move || {
            while switch.enabled_saying(&said) != Ok(true) {
                std::thread::sleep(look);
            }
            start();
        });
    if let Err(error) = waits {
        aterm_log::warn!(
            "Commons waits for [swarm] enabled, and its wait could not start ({error}): it starts \
             at aterm's next launch with sharing on"
        );
    }
    false
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
    use std::time::Duration;

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
        let (started, heard) = std::sync::mpsc::channel();
        let at_once = super::when_sharing_on(home, Duration::from_millis(10), move || {
            let _ = started.send(());
        });
        assert!(!at_once, "swarm off: no Commons runtime now");
        assert!(heard.try_recv().is_err(), "nor started by the wait");
        std::fs::write(&config, "[swarm]\nenabled = true\n").unwrap();
        heard
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("turned on: the runtime starts");
        let (started, heard) = std::sync::mpsc::channel();
        assert!(super::when_sharing_on(
            home,
            Duration::from_millis(10),
            move || {
                let _ = started.send(());
            }
        ));
        assert!(heard.try_recv().is_ok(), "swarm on: at once");
    }
}
