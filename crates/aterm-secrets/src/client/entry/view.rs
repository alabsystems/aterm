// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authenticate recipient-visible entries before looking up a particular name.

use super::*;

#[cfg(test)]
std::thread_local! {
    pub(super) static SKIP_STATE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(super) static SKIP_ROSTER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Clone, Copy)]
pub(super) enum Request<'a> {
    View,
    Entry { name: &'a str, reading: Reading },
}

pub(super) struct View {
    pub(super) state: State,
    pub(super) entries: Vec<Entry>,
    pub(super) version: VaultVersion,
    pub(super) is_admin: bool,
    pub(super) trusted_log: Option<crate::membership_log::Log>,
    pub(super) selected_log: Option<super::super::log_entries::LogSelection>,
    pub(super) envelope_context: Option<super::super::version::EnvelopeVersion>,
}

/// The caller retains custody. `Request::View` authenticates the ordinary reader's
/// complete recipient view without probing an invented entry name.
pub(super) fn read(
    paths: &Paths,
    expected: &Config,
    request: Request<'_>,
    _observation: &StateLock,
) -> crate::Result<Option<View>> {
    let (reading, requested, reading_roster) = match request {
        Request::View => (Reading::Materialized, None, false),
        Request::Entry { name, reading } => (
            reading,
            Some([name, crate::SWARM_ROSTER_ENTRY]),
            name == crate::SWARM_ROSTER_ENTRY,
        ),
    };
    let materialized = matches!(
        reading,
        Reading::Materialized | Reading::SigningMaterial | Reading::PublicationMaterial
    );
    let requested = if materialized {
        None
    } else {
        requested.as_ref().map(|names| names.as_slice())
    };
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    if config != *expected || !pin_admitted(&config, None) {
        return Ok(None);
    }
    let state = load_state(paths)?;
    #[cfg(test)]
    let check_state = !SKIP_STATE.get();
    #[cfg(not(test))]
    let check_state = true;
    if check_state && (state.membership != Stored::In || state.failed || !state.pending.is_empty())
    {
        return Ok(None);
    }
    let Some(identity) = read_identity(paths)? else {
        return Ok(None);
    };
    // Selection follows shared-register and anchor resolution. A named
    // reserved read also retains the roster needed for recipient policy.
    let (entries, envelope_digest, is_admin, trusted_log, selected_log, envelope_context) =
        if state.digest.is_none() && fsx::kind(&paths.cache_file())? == fsx::Kind::Missing {
            if state.source_identity != Some(source_identity(&config)) {
                return Ok(None);
            }
            let Some(configured) =
                super::super::log_entries::trusted_configured_log(paths, &config)?
            else {
                return Ok(None);
            };
            if !log_entry_reader_allowed(&configured.log, &identity) {
                return Ok(None);
            }
            let selected = super::super::log_entries::SelectedVaultEntries::from_log(
                configured.log.open_selection(&identity, requested)?,
                *configured.log.genesis().id(),
            );
            let Some(entries) = selected.entries else {
                return Ok(None);
            };
            let digest = *configured.log.genesis().id();
            (
                entries,
                digest,
                false,
                Some(configured.log),
                selected.log,
                None,
            )
        } else {
            if !source_bound(&config, &state) {
                return Ok(None);
            }
            let Some(cached) = read_held_copy(paths, &config, &state)? else {
                return Ok(None);
            };
            let envelope = envelope::parse(&cached)?;
            let seeded = super::super::log_entries::trusted_seeded_log(paths, &config, &envelope)?;
            if !super::super::log_entries::applied_log_available(
                &state,
                seeded.as_ref().map(|(_, digest)| *digest),
            ) {
                return Ok(None);
            }
            let selected = super::super::log_entries::select_entries_with_log(
                seeded.as_ref().map(|(log, digest)| (log, *digest)),
                &envelope,
                &identity,
                requested,
            )?;
            let Some(entries) = selected.entries else {
                return Ok(None);
            };
            let trusted_log = seeded.map(|(log, _)| log);
            // Reading the roster itself does not ask for admin standing.
            let admin = matches!(reading, Reading::ReservedAndAdmin)
                && holds_admin_locker(paths, &config, &envelope, &identity)?;
            (
                entries,
                envelope.digest(),
                admin,
                trusted_log,
                selected.log,
                Some(super::super::version::EnvelopeVersion {
                    seq: state.seq,
                    digest: envelope.digest(),
                }),
            )
        };
    #[cfg(test)]
    let check_roster = !SKIP_ROSTER.get();
    #[cfg(not(test))]
    let check_roster = true;
    if check_roster
        && !selected_entry_reader_allowed(
            &entries,
            trusted_log.as_ref(),
            &config,
            reading_roster,
            identity.recipient(),
        )
    {
        return Ok(None);
    }
    let log_ids = trusted_log
        .as_ref()
        .map(|log| std::sync::Arc::new(log.records().map(|record| *record.id()).collect()));
    let version = VaultVersion {
        mailbox_stamp: public_state_stamp(&config, &state, identity.recipient()),
        config,
        recipient: identity.recipient(),
        seq: state.seq,
        envelope: envelope_digest,
        log_ids,
    };
    Ok(Some(View {
        state,
        entries,
        version,
        is_admin,
        trusted_log,
        selected_log,
        envelope_context,
    }))
}
