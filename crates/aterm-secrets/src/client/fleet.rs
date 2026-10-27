// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authenticated ALab fleet-code distribution. The code is never materialized
//! as a secret file, logged, or included in an error.

use super::*;

/// Reserved vault entry holding the default ALab fleet's join code.
pub const FLEET_CODE_ENTRY: &str = "aterm-fleet-code";

/// Read the current signed ALab fleet code after the local apply completed.
/// Open-admitted ALab members qualify under the owner's 2026-10-26 default
/// full-membership policy. Other vaults never supply the default fleet.
/// `None`: the applied copy carries no code for this machine (none is
/// published, or the machine is not yet a member it reaches).
///
/// # Errors
/// Local state, the signed cache or its recipient lockers cannot be read safely,
/// or the apply has not finished (a pass applying now, or the last one leaving
/// an entry failed or pending), so the code is read once it has.
pub fn alab_fleet_code(paths: &Paths) -> crate::Result<Option<SecretBytes>> {
    let expected =
        config(paths)?.ok_or_else(|| Error::new("no vault configured; run aterm secrets join"))?;
    if !alab_vault(&expected) {
        return Ok(None);
    }
    reserved_code(paths, &expected)
}

/// [`alab_fleet_code`]'s read of `expected`'s applied copy, any vault's: an
/// apply that has not finished is an error naming it, never read as no code.
pub(super) fn reserved_code(
    paths: &Paths,
    expected: &Config,
) -> crate::Result<Option<SecretBytes>> {
    if let Some(entry) = entry::read_reserved_entry(paths, expected, FLEET_CODE_ENTRY)? {
        return Ok(Some(entry.into_parts().0));
    }
    let busy = StateLock::observe(paths)?.is_none()
        && fsx::kind(&paths.lock_file())? != fsx::Kind::Missing;
    if busy {
        return Err(Error::new(
            "a vault pass is applying on this machine now: the default fleet code is read once \
             it ends",
        ));
    }
    let state = load_state(paths)?;
    if state.membership == Stored::In && (state.failed || !state.pending.is_empty()) {
        return Err(Error::new(
            "this machine's last vault apply did not finish (`aterm secrets sync` names what \
             failed): the default fleet code is read once it does",
        ));
    }
    Ok(None)
}

/// Publish the existing fleet issuer's code to every current and future ALab
/// vault member, including open-admitted machines. This changes no fleet or
/// drive setting on the publisher.
///
/// # Errors
/// The caller is not an ALab vault admin, or publication fails.
pub fn admin_publish_alab_fleet(paths: &Paths, code: &SecretBytes) -> crate::Result<Published> {
    let admin = admin_open(paths)?;
    if !alab_vault(&admin.config) {
        return Err(Error::new(
            "only an ALab vault admin can publish the default fleet",
        ));
    }
    if code.is_empty() {
        return Err(Error::new("the fleet join code is empty"));
    }
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, FLEET_CODE_ENTRY, code.as_slice())
        .map_err(|error| Error::new(error.to_string()))?;
    admin_publish(paths, &admin, &doc)
}
