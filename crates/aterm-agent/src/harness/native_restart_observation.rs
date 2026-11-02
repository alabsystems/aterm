// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Passive client persistence contains bounded data only, never a mutation ticket or FD.

use super::super::native_observe::{self, ObservationBinding, Registration};
use super::*;

struct Observed {
    registration: Registration,
    binding: Option<ObservationBinding>,
}

fn path(opts: &Opts, tab: &str) -> std::path::PathBuf {
    opts.state
        .join("native-observations")
        .join(aterm_codec::hex::encode_lower(tab.as_bytes()))
}

fn read(opts: &Opts, tab: &str) -> Option<Observed> {
    use std::io::Read;
    let mut bytes = String::new();
    std::fs::File::open(path(opts, tab))
        .ok()?
        .take((native_observe::MAX_REQUEST * 2 + 1) as u64)
        .read_to_string(&mut bytes)
        .ok()?;
    if bytes.len() > native_observe::MAX_REQUEST * 2 {
        return None;
    }
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

fn save(opts: &Opts, tab: &str, observed: &Observed) -> std::io::Result<()> {
    let request = native_observe::Request::Register(observed.registration.clone())
        .encode()
        .ok_or_else(|| std::io::Error::other("invalid passive registration"))?;
    let binding = match &observed.binding {
        None => "-".into(),
        Some(binding) => native_observe::Request::Refresh(binding.clone())
            .encode()
            .ok_or_else(|| std::io::Error::other("invalid passive binding"))?,
    };
    let path = path(opts, tab);
    std::fs::create_dir_all(path.parent().expect("observation directory"))?;
    let body = format!("{request}\n{binding}\n");
    #[cfg(unix)]
    {
        aterm_uds::atomic_file::write(&path, body.as_bytes(), 0o600)
    }
    #[cfg(not(unix))]
    {
        super::super::upgrade_models::write_atomic(&path, &body)
    }
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
    if !constructor_enabled(&mut client, &snap.tab) {
        return;
    }
    let Ok(Reply::Info { epoch, .. }) =
        native_act::exchange(&mut client, &snap.tab, &Request::Info)
    else {
        return;
    };
    let mut observed = match read(opts, &snap.tab) {
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
    if observed.binding.is_none() && save(opts, &snap.tab, &observed).is_err() {
        return;
    }
    match native_observe::exchange(&mut client, &snap.tab, &request) {
        Ok(native_observe::Reply::Registered(binding)) => {
            if observed.binding.as_ref() != Some(&binding) {
                observed.binding = Some(binding);
                let _saved = save(opts, &snap.tab, &observed);
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
                let _saved = save(opts, &snap.tab, &observed);
            }
        }
        _ => {}
    }
}

pub(super) fn captured(
    opts: &Opts,
    snap: &super::super::relaunch::Snapshot,
) -> Option<(Registration, ObservationBinding)> {
    let observed = snap
        .session
        .as_deref()
        .and_then(|session| upgrade_drive::load(opts, session))
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
        .or_else(|| read(opts, &snap.tab))?;
    let binding = observed.binding?;
    let process = &observed.registration.vendor;
    (process.pid == snap.pid
        && process.parent == snap.shell
        && Image::at(&snap.program).as_ref() == Some(&process.image))
    .then_some((observed.registration, binding))
}
