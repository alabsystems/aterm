// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The F10 interim handover (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.3.4 step 9): the
//! Allowing member's `[everyone]` files go to the new Mac inside the CPace channel, and the new
//! Mac writes its vault configuration and `~/.secrets` from them. It holds a snapshot until an
//! admin's approver admits the request its stored [`Allowed`] rides on ([`request_join`]).

use super::{
    Config, Paths, StateLock, Stored, apply_entries, apply_failed, config, load_state,
    machine_identity, read_identity, save_config, save_state, source_bound, source_identity,
};
use crate::envelope::{self, Floor, Pin, Verdict};
use crate::materialize::Report;
use crate::members::bundle::Handed;
use crate::members::{Allowed, Bundle};
use crate::text::{self, Audience, Entry};
use crate::transport::{self, Source};
use crate::{Error, age, fsx};
use std::collections::BTreeSet;

/// The file a new Mac keeps the [`Allowed`] it was handed in, re-sent with each join request.
fn allowed_file(paths: &Paths) -> std::path::PathBuf {
    paths.dir.join("allowed")
}

/// The [`Allowed`] this Mac was handed, re-sent with its join requests, if it holds one.
pub(super) fn stored_allowed(paths: &Paths) -> Option<String> {
    let bytes = fsx::read_private(&allowed_file(paths), 4096).ok()??;
    let text = std::str::from_utf8(bytes.as_slice())
        .ok()?
        .trim()
        .to_owned();
    Allowed::from_text(&text).ok().map(|_| text)
}

/// Whether this Mac holds an [`Allowed`] a member handed it: a member let it in, and it waits
/// for the vault to record that (`aterm members status`).
#[must_use]
pub fn holds_allowed(paths: &Paths) -> bool {
    stored_allowed(paths).is_some()
}

/// Forget the handed [`Allowed`]: this Mac is in (for a Mac the membership log let in, once an
/// admin's approver named it in `[machines]`), or now joins another vault.
pub(super) fn forget_allowed(paths: &Paths) {
    let _ = std::fs::remove_file(allowed_file(paths));
}

/// Keep the [`Allowed`] a member handed this Mac, for its join requests ([`stored_allowed`]).
///
/// # Errors
/// The file cannot be written.
pub(super) fn keep_allowed(paths: &Paths, allowed: &Allowed) -> crate::Result<()> {
    fsx::write_atomic(
        &allowed_file(paths),
        format!("{}\n", allowed.to_text()).as_bytes(),
        0o600,
    )
}

/// The bundle this member hands the Mac `allowed` names: its applied vault's name, pin and
/// server, and its `[everyone]` entries (the Commons control record and AI CLI logins, which
/// no vault carries to another Mac, left out; an ` if-absent` entry whose anchor is left out
/// goes with it).
///
/// # Errors
/// This Mac holds no applied signed vault it reads from a URL, or its cache does not verify,
/// or a vault pass here held this Mac's state for [`HANDOVER_LOCK_WAIT`].
pub fn handover_bundle(paths: &Paths, allowed: &Allowed) -> crate::Result<Bundle> {
    handover_bundle_until(
        paths,
        allowed,
        std::time::Instant::now() + HANDOVER_LOCK_WAIT,
    )
}

/// How long a handover waits for a vault pass on this Mac to let go of its state. The whole
/// `ask` exchange, this wait included, runs inside the fleet listener's handshake watchdog
/// (`aterm_net::fleet::HANDSHAKE_WITHIN`, 3 s), which cuts the member's socket at its deadline,
/// and the joining Mac reads each line within 5 s of its own; a wait past that bound ends the run
/// unheard (relayed, it writes the bundle into the node job's socket after the joiner left). So
/// this waits out a short pass and leaves the rest of the bound to the TLS handshake, CPace and
/// the possession proof, and a longer pass ends the run with the refusal that says to Allow
/// again, which reaches the new Mac. A run's phrase burns on any error.
pub const HANDOVER_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

/// [`handover_bundle`], waiting for this Mac's state until `deadline`.
fn handover_bundle_until(
    paths: &Paths,
    allowed: &Allowed,
    deadline: std::time::Instant,
) -> crate::Result<Bundle> {
    let none = || Error::new("this Mac holds no applied vault to hand a new Mac");
    let Some(_observation) = StateLock::observe_until(paths, deadline)? else {
        // `observe_until` gives up at once when there is no lock file: no vault was ever
        // joined here. With one, a pass still held the state at the deadline.
        return Err(if paths.lock_file().exists() {
            Error::new(
                "a vault pass on this Mac still holds its state; Allow the new Mac again once it ends",
            )
        } else {
            none()
        });
    };
    let config = config(paths)?.ok_or_else(none)?;
    let pin = config.signer.ok_or_else(none)?;
    let Source::Url(url) = &config.source else {
        return Err(Error::new(
            "this Mac reads its vault from a path, which a new Mac cannot reach",
        ));
    };
    let state = load_state(paths)?;
    if state.membership != Stored::In
        || state.failed
        || !state.pending.is_empty()
        || (!source_bound(&config, &state) && !super::log_only_source_bound(&config, &state))
    {
        return Err(none());
    }
    let identity = read_identity(paths)?.ok_or_else(none)?;
    if super::steward::local_recipient_denied_in_roster(paths) {
        return Err(none());
    }
    if let Some(why) = super::admit::roster_keeps_out(paths, None, allowed.binding(), [0; 32]) {
        return Err(Error::new(why));
    }
    let check_log = |log: &crate::membership_log::Log| -> crate::Result<()> {
        let me = identity.recipient();
        let roster = super::log_entries::local_swarm_roster_for_log(paths, log);
        if log.recipient_is_removed(&me)
            || log.audience_left_out_recipient(&crate::membership_log::EpochAudience::Everyone, &me)
            || super::log_entries::recipient_denied_by_roster_or_log(roster.as_ref(), log, &me)
        {
            return Err(none());
        }
        // A removed Mac's "Allow again" restores it only by the log's restoring `Admit`, which
        // F10's files do not carry: here no press lets it in.
        if let Some(why) = super::admit::kept_out(log, allowed.binding(), false) {
            return Err(Error::new(why));
        }
        if let Some(already) = log.membership().get(&allowed.binding().actor())
            && roster
                .as_ref()
                .is_some_and(|r| super::log_entries::member_denied_by_roster(r, already))
        {
            return Err(Error::new(
                "the vault removed this Mac, and an Allow does not let a removed Mac back in",
            ));
        }
        Ok(())
    };
    if let Some(log) = super::log_entries::trusted_local_log(paths, &config)? {
        check_log(&log)?;
    }
    let mut everyone: Vec<Entry> = Vec::new();
    let mut from_log = false;
    if let Some(digest) = state.digest {
        let bytes = super::held_copy(paths, &config, &state)
            .or_else(|| {
                fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)
                    .ok()
                    .flatten()
                    .map(|b| b.as_slice().to_vec())
            })
            .ok_or_else(none)?;
        let (envelope, pinned, verdict) = envelope::verify_for_reader(
            bytes.as_slice(),
            &config.name,
            &pin,
            &config.followed,
            Some(&Floor::version(state.seq, digest)),
        )?;
        if verdict != Verdict::Unchanged || !matches!(pinned, Pin::Same) {
            return Err(none());
        }
        if let Some((log, _)) =
            super::log_entries::readable_log(paths, &config, &envelope, &identity)?
        {
            check_log(&log)?;
            let opened = log.open_audience_entries(&identity)?.ok_or_else(none)?;
            if opened
                .fully_unwrapped_audiences
                .contains(&crate::membership_log::EpochAudience::Everyone)
            {
                from_log = true;
                if let Some(map) = opened.by_audience.get(&Audience::Everyone) {
                    everyone.extend(
                        map.values()
                            .filter(|entry| {
                                !crate::reserved_name(&entry.name)
                                    && text::cli_login(&entry.name).is_none()
                            })
                            .cloned(),
                    );
                }
            }
        }
        if !from_log {
            for locker in envelope
                .lockers
                .iter()
                .filter(|locker| locker.label != "admin")
            {
                let plain = match age::decrypt(
                    std::slice::from_ref(&identity),
                    locker.armored.as_bytes(),
                ) {
                    Ok(plain) => plain,
                    Err(age::AgeError::NoMatch) => continue,
                    Err(error) => {
                        return Err(Error::new(format!("locker {}: {error}", locker.label)));
                    }
                };
                let entries = text::parse_locker(plain.as_slice())
                    .map_err(|error| Error::new(format!("locker {}: {error}", locker.label)))?;
                everyone.extend(entries.into_iter().filter(|entry| {
                    entry.audience == Audience::Everyone
                        && !crate::reserved_name(&entry.name)
                        && text::cli_login(&entry.name).is_none()
                }));
            }
        }
    } else if state.records_digest.is_some()
        && let Some((store, records_digest)) =
            super::log_entries::trusted_seeded_store(paths, &config, &pin, &[])?
        && Some(records_digest) == state.records_digest
        && let Some(log) = store.log()
    {
        check_log(log)?;
        let opened = log.open_audience_entries(&identity)?.ok_or_else(none)?;
        if !opened
            .fully_unwrapped_audiences
            .contains(&crate::membership_log::EpochAudience::Everyone)
        {
            return Err(none());
        }
        if let Some(map) = opened.by_audience.get(&Audience::Everyone) {
            everyone.extend(
                map.values()
                    .filter(|entry| {
                        !crate::reserved_name(&entry.name) && text::cli_login(&entry.name).is_none()
                    })
                    .cloned(),
            );
        }
    } else {
        return Err(none());
    }
    let names: BTreeSet<String> = everyone.iter().map(|entry| entry.name.clone()).collect();
    everyone.retain(|entry| {
        entry
            .anchor
            .as_ref()
            .is_none_or(|anchor| names.contains(anchor))
    });
    Ok(Bundle {
        vault: config.name.clone(),
        signer: pin,
        followed: config.followed.clone(),
        servers: bundle_servers(paths, &config, url),
        allowed: allowed.clone(),
        handed: Handed::Files(text::render_everyone_locker(&everyone)),
    })
}

/// The servers a bundle names: `url`, this Mac's source, first, then the policy's, at most 8.
pub(super) fn bundle_servers(paths: &Paths, config: &Config, url: &str) -> Vec<String> {
    let mut servers = vec![url.to_owned()];
    for cand in super::policy_peer_urls(paths, config) {
        if servers.len() >= 8 {
            break;
        }
        if (cand.starts_with("http://") || cand.starts_with("https://"))
            && cand.len() <= 512
            && cand.bytes().all(|b| b.is_ascii_graphic())
            && !servers.contains(&cand)
        {
            servers.push(cand);
        }
    }
    servers
}

/// Post the request `allowed` names, carrying it, to this member's vault servers, so an admin's
/// approver admits it even while the new Mac cannot reach one.
///
/// # Errors
/// This Mac reads no vault from a URL, or every server refused the request.
pub fn post_allowed(paths: &Paths, allowed: &Allowed) -> crate::Result<()> {
    let config = config(paths)?.ok_or_else(|| Error::new("no vault configured"))?;
    let Source::Url(primary_url) = &config.source else {
        return Ok(());
    };
    let recipient = allowed.binding().recipient();
    let line = transport::JoinRequest {
        recipient: recipient.to_bech32(),
        host: allowed.name().to_owned(),
        code: super::join_code(&recipient),
        time: None,
        lan: false,
        allowed: Some(allowed.to_text()),
        proof: None,
    }
    .line();
    let mut urls = super::ledger_publish::candidate_server_urls(paths, &config, &[]);
    if urls.is_empty() {
        urls.push(primary_url.clone());
    }
    let mut any_ok = false;
    let mut first_err = None;
    for url in &urls {
        match transport::post_join(url, &line) {
            Ok(()) => any_ok = true,
            Err(err) if first_err.is_none() => first_err = Some(err),
            Err(_) => {}
        }
    }
    if any_ok {
        Ok(())
    } else {
        Err(first_err.unwrap_or_else(|| Error::new("no vault server configured")))
    }
}

/// Ask the vault server this Mac's bundle named to let it in, carrying the [`Allowed`] it was
/// handed, so an admin's approver admits it whenever one is reachable.
///
/// # Errors
/// No vault is configured, or the server could not be reached.
pub fn ask_allowed(paths: &Paths) -> crate::Result<String> {
    let config = config(paths)?.ok_or_else(|| Error::new("no vault configured"))?;
    super::request_join(paths, &config, None, super::Asker::Again)
}

/// Write this Mac's vault configuration and `~/.secrets` from a member's join bundle: the
/// vault's source, name, pin and the name it was allowed under, and the bundle's files, each
/// recorded as the vault's so the admission's first apply finds them unchanged. Membership
/// stays pending, and the bundle's [`Allowed`] is kept for this Mac's join requests.
///
/// # Errors
/// This Mac is already in a vault, its key is not the one the bundle allows, the bundle's files
/// are not one `[everyone]` locker, or a write failed.
pub fn write_from_bundle(paths: &Paths, bundle: &Bundle) -> crate::Result<Report> {
    let Handed::Files(files) = &bundle.handed else {
        return Err(Error::new(
            "the member's bundle carries no [everyone] files",
        ));
    };
    let _lock = StateLock::take(paths)?;
    let current = config(paths)?;
    let mut state = load_state(paths)?;
    if current.is_some() && state.membership == Stored::In {
        return Err(Error::new(
            "this Mac is already in a vault; leave it first (aterm secrets leave)",
        ));
    }
    if machine_identity(paths)?.recipient() != bundle.allowed.binding().recipient() {
        return Err(Error::new("the member's bundle allows another Mac's key"));
    }
    let entries = text::parse_locker(files.as_slice())
        .map_err(|error| Error::new(format!("the member's bundle: {error}")))?;
    if entries
        .iter()
        .any(|entry| entry.audience != Audience::Everyone)
    {
        return Err(Error::new(
            "the member's bundle holds more than its [everyone] files",
        ));
    }
    let server = bundle
        .servers
        .first()
        .cloned()
        .ok_or_else(|| Error::new("the member's bundle names no vault server"))?;
    let config = Config {
        source: Source::Url(server),
        name: bundle.vault.clone(),
        signer: Some(bundle.signer),
        followed: bundle.followed.clone(),
        host: bundle.allowed.name().to_owned(),
    };
    save_config(paths, &config)?;
    let report = apply_entries(paths, &mut state, &entries)?;
    state.membership = Stored::Pending;
    state.failed = apply_failed(&report);
    state.proof = None;
    state.refused = None;
    state.source_identity = Some(source_identity(&config));
    save_state(paths, &state)?;
    keep_allowed(paths, &bundle.allowed)?;
    Ok(report)
}

/// Hand the Mac at `joiner` the Allow `member` signs for it under `host`, as a run does, so its
/// join requests carry it: the tests' stand-in for a person's Allow and the typed phrase.
#[cfg(test)]
pub(crate) fn test_hand_allowed(member: &Paths, joiner: &Paths, host: &str) {
    let bootstrap = crate::commons::bootstrap::Bootstrap::cached(member)
        .expect("member state")
        .expect("a Commons member");
    let trust = bootstrap.trust();
    machine_identity(joiner).expect("identity");
    let device = crate::commons::bootstrap::Device::open(joiner)
        .expect("device")
        .expect("an identity");
    let request = device
        .request(trust.network, trust.authority)
        .expect("request");
    let allowed = bootstrap
        .device()
        .with_seed(|seed| Allowed::sign(request.binding(), host, seed))
        .expect("signed");
    fsx::ensure_private_dir(&joiner.dir).expect("dir");
    fsx::write_atomic(
        &allowed_file(joiner),
        format!("{}\n", allowed.to_text()).as_bytes(),
        0o600,
    )
    .expect("written");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An Allow for a fresh synthetic Mac, signed with a synthetic writer seed.
    fn synthetic_allowed(root: &std::path::Path) -> Allowed {
        let joiner = Paths::for_home(&root.join("joiner"));
        machine_identity(&joiner).expect("identity");
        let request = crate::commons::bootstrap::Device::open(&joiner)
            .expect("device")
            .expect("an identity")
            .request([1; 16], [2; 32])
            .expect("request");
        Allowed::sign(request.binding(), "m10", &[7; 32]).expect("signed")
    }

    /// A member whose state a vault pass holds past the wait says so; it does not claim to hold
    /// no vault, which sent the person looking for a vault that was there.
    ///
    /// NEGATIVE CONTROL: map a held lock to `none()` as before (the `Err(if …)` arm returning
    /// `none()` alone): the busy assertion fails with "this Mac holds no applied vault".
    #[test]
    fn a_member_a_pass_holds_says_it_is_busy_not_that_it_holds_no_vault() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let allowed = synthetic_allowed(root.path());
        let member = Paths::for_home(&root.path().join("member"));

        let never = handover_bundle_until(&member, &allowed, std::time::Instant::now())
            .expect_err("a Mac that never joined has nothing to hand");
        assert!(
            never.to_string().contains("holds no applied vault"),
            "{never}"
        );

        let pass = StateLock::take(&member).expect("a pass holds the state");
        let busy = handover_bundle_until(&member, &allowed, std::time::Instant::now())
            .expect_err("the pass still holds the state at the deadline");
        assert!(busy.to_string().contains("still holds its state"), "{busy}");
        assert!(!busy.to_string().contains("no applied vault"), "{busy}");
        drop(pass);
    }
}
