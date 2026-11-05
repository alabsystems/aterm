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
mod tests {
    use super::*;
    use crate::client::{Config, State, Stored, save_config, save_state};
    use crate::transport::Source;
    use std::os::unix::fs::PermissionsExt;

    fn configured(home: &std::path::Path) -> Paths {
        let paths = Paths::for_home(home);
        let signer = crate::b64::decode_padded(crate::ALAB_SIGNER).unwrap();
        save_config(
            &paths,
            &Config {
                source: Source::Url("https://example.invalid/vault".into()),
                mirrors: Vec::new(),
                name: crate::ALAB_VAULT.into(),
                signer: Some(signer.try_into().unwrap()),
                followed: Vec::new(),
                host: "member".into(),
            },
        )
        .unwrap();
        paths
    }

    #[test]
    fn absent_subscriber_admission_reads_no_strict_materialization_lock() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = configured(root.path());
        let lock = paths.lock_file();
        std::fs::write(&lock, b"old lock").unwrap();
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(super::super::materialization_ready(&paths).is_err());
        assert!(configured_for_recovery(&paths).unwrap());
        let repaired = after_held(HeldMend::NotHeld, || {
            assert!(configured_for_recovery(&paths)?);
            Ok(HeldMend::Moved("installed".into(), HeldMoved::Missing))
        })
        .unwrap();
        assert_eq!(
            repaired,
            HeldMend::Moved("installed".into(), HeldMoved::Missing)
        );
        assert_eq!(
            std::fs::metadata(&lock).unwrap().permissions().mode() & 0o7777,
            0o644
        );
        assert_eq!(std::fs::read(&lock).unwrap(), b"old lock");

        save_state(
            &paths,
            &State {
                membership: Stored::Revoked,
                ..State::default()
            },
        )
        .unwrap();
        assert!(!configured_for_recovery(&paths).unwrap());
        save_state(&paths, &State::default()).unwrap();
        std::fs::write(
            root.path().join(".config/aterm/aterm.toml"),
            "[swarm]\nenabled = false\n",
        )
        .unwrap();
        assert!(!configured_for_recovery(&paths).unwrap());
    }

    #[test]
    fn an_existing_held_decision_never_falls_through_to_absent_recovery() {
        for kept in [
            HeldMend::Kept(HeldKept::Pinned),
            HeldMend::Kept(HeldKept::Reads),
            HeldMend::Unread("busy".into()),
            HeldMend::Moved("already moved".into(), HeldMoved::ManagedRelease),
        ] {
            assert_eq!(
                after_held(kept.clone(), || panic!("must preserve held decision")).unwrap(),
                kept
            );
        }
    }
}
