// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Passive client persistence contains bounded data only, never a mutation ticket or FD.

use super::super::native_observe::{self, ObservationBinding, Registration};
use super::*;

pub(super) struct Observed {
    pub(super) registration: Registration,
    pub(super) binding: Option<ObservationBinding>,
}

fn is_owned_plain_dir(meta: &std::fs::Metadata) -> bool {
    if !meta.file_type().is_dir() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != aterm_uds::peer::our_uid() {
            return false;
        }
    }
    true
}

fn existing_owned_plain_dir(dir: &std::path::Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if is_owned_plain_dir(&meta) => Ok(true),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{}: not this user's plain directory", dir.display()),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn ensure_observation_dir(state: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    if !existing_owned_plain_dir(state)? {
        std::fs::create_dir_all(state)?;
        if !existing_owned_plain_dir(state)? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{}: not this user's plain directory", state.display()),
            ));
        }
    }
    let dir = state.join("native-observations");
    if !existing_owned_plain_dir(&dir)? {
        std::fs::create_dir_all(&dir)?;
        if !existing_owned_plain_dir(&dir)? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{}: not this user's plain directory", dir.display()),
            ));
        }
    }
    Ok(dir)
}

fn path(state: &std::path::Path, tab: &str) -> std::path::PathBuf {
    state
        .join("native-observations")
        .join(aterm_codec::hex::encode_lower(tab.as_bytes()))
}

/// The records whose observers are at work in this process ([`Turn`]).
static OBSERVING: std::sync::Mutex<std::collections::BTreeSet<std::path::PathBuf>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// One observer's turn at a tab's record: from its read of the record to its last save of it
/// ([`observe_live`]), no other observer of this process reads it to change it or saves it
/// ([`save`] takes the turn). Several threads observe one tab (the snapshot read that lands
/// late, the record follow, the worker), and a save is a temporary flushed durably and then
/// renamed: without the turn, an observer whose flush is slow renames the state it read before
/// over the binding another observer had acknowledged since. Once the vendor is dead nothing
/// writes the record again, so its crash has no proof and its relaunch waits on one for good
/// (measured 2026-11-13 on a loaded Mac: a record renamed 0.76 s after its temporary was
/// written, over an acknowledged binding, 0.1 s after the restart that ended the vendor).
pub(super) struct Turn(std::path::PathBuf);

impl Turn {
    /// The turn at `tab`'s record under `state`, or `None` while another observer has it: that
    /// one's result stands, and the next observation takes its own turn.
    pub(super) fn take(state: &std::path::Path, tab: &str) -> Option<Self> {
        let record = path(state, tab);
        // The lock is given back before a turn exists: a turn made and dropped under it (the
        // refused one) would take the lock again in its drop, and end the other observer's turn.
        let taken = OBSERVING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(record.clone());
        taken.then(|| Self(record))
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        OBSERVING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0);
    }
}

pub(super) fn read(state: &std::path::Path, tab: &str) -> Option<Observed> {
    if !existing_owned_plain_dir(state).ok()?
        || !existing_owned_plain_dir(&state.join("native-observations")).ok()?
    {
        return None;
    }
    let bytes = aterm_uds::runs::read_bounded_regular_text(
        &path(state, tab),
        (native_observe::MAX_REQUEST * 2) as u64,
    )
    .ok()?;
    let mut lines = bytes.lines();
    let native_observe::Request::Register(registration) =
        native_observe::Request::parse(lines.next()?)?
    else {
        return None;
    };
    let binding = match lines.next()? {
        "-" => None,
        text => {
            let native_observe::Request::Refresh(binding) = native_observe::Request::parse(text)?
            else {
                return None;
            };
            if binding.id != registration.id || binding.host_epoch != registration.host_epoch {
                return None;
            }
            Some(binding)
        }
    };
    lines.next().is_none().then_some(Observed {
        registration,
        binding,
    })
}

/// Save `observed` as the record `turn` is the turn at.
pub(super) fn save(turn: &Turn, observed: &Observed) -> std::io::Result<()> {
    let request = native_observe::Request::Register(observed.registration.clone())
        .encode()
        .ok_or_else(|| std::io::Error::other("invalid passive registration"))?;
    let binding = match &observed.binding {
        None => "-".into(),
        Some(binding) => native_observe::Request::Refresh(binding.clone())
            .encode()
            .ok_or_else(|| std::io::Error::other("invalid passive binding"))?,
    };
    let path = &turn.0;
    let state = path
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or_else(|| std::io::Error::other("a record outside its directory"))?;
    ensure_observation_dir(state)?;
    let body = format!("{request}\n{binding}\n");
    #[cfg(unix)]
    {
        aterm_uds::atomic_file::write(path, body.as_bytes(), 0o600)
    }
    #[cfg(not(unix))]
    {
        super::super::atomic_write::write_atomic(path, &body)
    }
}

fn observation_kind_enabled(enabled: &[Constructor]) -> bool {
    enabled.contains(&Constructor::PrivateRestart) || enabled.contains(&Constructor::ColdRestart)
}

/// Called by the real snapshot worker while the vendor is live. The first snapshot may
/// precede host publication; refusal simply leaves crash entry without proof until captured.
pub(crate) fn observe_live(opts: &Opts, snap: &super::super::relaunch::Snapshot) {
    if opts.token.is_none()
        || opts.dry_run
        || !matches!(snap.run, super::super::relaunch::AgentRun::Claude)
    {
        return;
    }
    let Some(process) = Process::observe(snap.pid) else {
        return;
    };
    let Some(shell) = Process::observe(snap.shell) else {
        return;
    };
    if process.parent != shell.pid || Image::at(&snap.program).as_ref() != Some(&process.image) {
        return;
    }
    let Ok(mut client) = upgrade_drive::connect(opts, &snap.tab) else {
        return;
    };
    let Ok(Reply::Constructors(enabled)) =
        native_act::exchange(&mut client, &snap.tab, &Request::Constructors)
    else {
        return;
    };
    // A passive death witness grants no account or execution authority. Stored,
    // provider and custom-directory routes all need the same live identity proof;
    // their current credential context is admitted separately before Execute.
    if !observation_kind_enabled(&enabled) {
        return;
    }
    let Ok(Reply::Info { epoch, .. }) =
        native_act::exchange(&mut client, &snap.tab, &Request::Info)
    else {
        return;
    };
    // Taken before the record is read and held to the last save: see [`Turn`].
    let Some(turn) = Turn::take(&opts.state, &snap.tab) else {
        return;
    };
    let mut observed = match read(&opts.state, &snap.tab) {
        Some(previous)
            if previous.registration.vendor == process
                && previous.registration.shell == shell
                && previous.registration.host_epoch == epoch =>
        {
            previous
        }
        _ => {
            let Ok(id) = native_act::operation_id() else {
                return;
            };
            Observed {
                registration: Registration {
                    id,
                    host_epoch: epoch,
                    vendor: process,
                    shell,
                },
                binding: None,
            }
        }
    };
    let request = observed.binding.clone().map_or_else(
        || native_observe::Request::Register(observed.registration.clone()),
        native_observe::Request::Refresh,
    );
    // Preserve the exact id/facts before dispatch; a lost registration ACK replays those facts.
    if observed.binding.is_none() && save(&turn, &observed).is_err() {
        return;
    }
    match native_observe::exchange(&mut client, &snap.tab, &request) {
        Ok(native_observe::Reply::Registered(binding)) => {
            if observed.binding.as_ref() != Some(&binding) {
                observed.binding = Some(binding);
                let _saved = save(&turn, &observed);
            }
        }
        Ok(native_observe::Reply::Unknown)
            if Process::observe(observed.registration.vendor.pid).as_ref()
                == Some(&observed.registration.vendor)
                && Process::observe(observed.registration.shell.pid).as_ref()
                    == Some(&observed.registration.shell) =>
        {
            // A changed guard can start a new passive capture only after a fresh positive
            // live observation. Death/unknown reads retain the old positive crash witness.
            if let Ok(id) = native_act::operation_id() {
                observed.registration.id = id;
                observed.binding = None;
                let _saved = save(&turn, &observed);
            }
        }
        _ => {}
    }
}

pub(super) fn captured(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
) -> Option<(Registration, ObservationBinding)> {
    let record = snap
        .session
        .as_deref()
        .map(|session| upgrade_drive::load_record(opts, session))
        .transpose()
        .ok()?
        .flatten();
    let observed = record
        .and_then(|state| state.native_observation)
        .filter(|saved| {
            saved.registration.vendor.pid == snap.pid
                && saved.registration.shell.pid == snap.shell
                && Image::at(&snap.program).as_ref() == Some(&saved.registration.vendor.image)
        })
        .map(|saved| Observed {
            registration: saved.registration,
            binding: Some(saved.binding),
        })
        .or_else(|| read(&opts.state, &snap.tab))?;
    let binding = observed.binding?;
    let process = &observed.registration.vendor;
    (process.pid == snap.pid
        && process.parent == snap.shell
        && Image::at(&snap.program).as_ref() == Some(&process.image))
    .then_some((observed.registration, binding))
}

#[cfg(test)]
#[path = "native_restart_observation_constructor_tests.rs"]
mod constructor_tests;

#[cfg(test)]
#[path = "native_restart_observation_turn_tests.rs"]
mod turn_tests;
