// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Recovery of the configured ALab subscriber, including an absent job after setup finished.

use super::{
    HeldKept, HeldMend, HeldMoved, Membership, Paths, alab_vault, config, locker_skew, status,
};
use crate::service::{self, Job};

/// Refresh a held subscriber, or create its missing managed ALab job.
/// Callers must hold their current setup choice through this local service work.
/// No vault is joined or credential materialized: pending members still need
/// ordinary authenticated admission. Off, pins and foreign existing jobs win.
pub fn mend_subscriber(paths: &Paths) -> crate::Result<HeldMend> {
    if config(paths)?.as_ref().is_some_and(alab_vault) && !configured_for_recovery(paths)? {
        return Ok(HeldMend::NotHeld);
    }
    let held = super::mend_held(paths, Job::Subscriber)?;
    after_held(held, || {
        if !locker_skew::own()?.release {
            return Ok(HeldMend::Kept(HeldKept::Development));
        }
        let mut admitted = || configured_for_recovery(paths);
        if !admitted()? {
            return Ok(HeldMend::NotHeld);
        }
        let program = service::program_for_service()?;
        if let Some(line) =
            service::refresh_subscriber_scheduling(&paths.home, &program, &mut admitted)?
        {
            return Ok(HeldMend::Moved(line, HeldMoved::Scheduling));
        }
        Ok(
            match service::repair_absent_subscriber(&paths.home, &program, &mut admitted) {
                Ok(line) => line.map_or(HeldMend::NotHeld, |line| {
                    HeldMend::Moved(line, HeldMoved::Missing)
                }),
                // Unlike held-job migration, creation never boots out a job. A
                // transient failure stays retryable on the next ordinary pass.
                Err(error) => HeldMend::Unread(error.to_string()),
            },
        )
    })
}

fn after_held(
    held: HeldMend,
    absent: impl FnOnce() -> crate::Result<HeldMend>,
) -> crate::Result<HeldMend> {
    match held {
        HeldMend::NotHeld => absent(),
        kept => Ok(kept),
    }
}

fn configured_for_recovery(paths: &Paths) -> crate::Result<bool> {
    if !crate::swarm::enabled_in_home(&paths.home)? {
        return Ok(false);
    }
    let Some(configured) = config(paths)? else {
        return Ok(false);
    };
    if !alab_vault(&configured) {
        return Ok(false);
    }
    let current = status(paths)?;
    Ok(current.config.as_ref() == Some(&configured)
        && !matches!(
            current.membership,
            Membership::Unconfigured | Membership::Revoked { .. }
        )
        && !super::local_recipient_denied_in_roster(paths))
}

#[cfg(test)]
#[path = "subscriber_tests.rs"]
mod tests;
