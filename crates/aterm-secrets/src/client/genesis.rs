// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm secrets genesis`: ALab's admin signs Genesis v2 once
//! (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.3.8, F12), with a released aterm, and the release
//! after it compiles the record's digest ([`crate::ALAB_GENESIS`]).
//!
//! The record lists every `[machines]` recipient of the published vault, with the binding
//! Commons' current roster holds for it, under its `aterm-swarm-roster` label. The `ALAB_SIGNER`
//! seed in the admin locker signs it, as provenance only: builds check it by its digest.

use super::*;
use crate::commons::bootstrap::{Availability, Bootstrap};
use crate::membership_log::genesis::{self, Machine};
use crate::membership_log::{Record, Root};
use crate::swarm_roster::{SheetHints, SwarmPolicy, next_free_number};

/// What [`admin_sign_genesis`] wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignedGenesis {
    /// The record's digest: ALab's network id, which F12b compiles in.
    pub digest: [u8; 32],
    /// How many machines it lists.
    pub members: usize,
}

/// Sign ALab's Genesis v2 into `out`, a file that must not exist yet, with the `ALAB_SIGNER` seed
/// from this Mac's admin locker. Only a released aterm signs it.
///
/// # Errors
/// This aterm is a dev build, `out` exists, this Mac is no admin of the ALab vault, the vault
/// has no Commons control or this Mac reads no current roster of it, or the record cannot be made
/// or written.
pub fn admin_sign_genesis(paths: &Paths, out: &Path) -> crate::Result<SignedGenesis> {
    let lineage = decode_key32(crate::ALAB_SIGNER)
        .ok_or_else(|| Error::new("this build's ALab vault key is unreadable"))?;
    let released = aterm_types::version::installed().is_some_and(|stamp| stamp.release_build);
    sign_genesis(paths, out, &lineage, released)
}

/// Sign and seed Genesis v2 into `out` for a non-ALab vault using the signer pinned in `paths`'s
/// `source.toml`.
///
/// # Errors
/// `paths` has no configured signer, it names the ALab vault, or [`sign_genesis`] fails.
pub fn seed_vault_genesis(paths: &Paths, out: &Path) -> crate::Result<SignedGenesis> {
    let cfg = config(paths)?.ok_or_else(|| Error::new("no vault configured"))?;
    if alab_vault(&cfg) {
        return Err(Error::new(
            "the ALab vault's Genesis v2 is signed with admin_sign_genesis",
        ));
    }
    let lineage = cfg
        .signer
        .ok_or_else(|| Error::new("a plain vault has no signer key"))?;
    sign_genesis(paths, out, &lineage, true)
}

/// Sign Genesis v2 for a scratch vault whose signer public key hex is `signer`. This seam is
/// absent from production release builds.
///
/// # Errors
/// `signer` is not a 32-byte hex key, or [`sign_genesis`] fails.
#[cfg(any(test, debug_assertions))]
pub fn test_sign_genesis(paths: &Paths, out: &Path, signer: &str) -> crate::Result<SignedGenesis> {
    let lineage =
        decode_key32(signer).ok_or_else(|| Error::new("test vault signer key is unreadable"))?;
    sign_genesis(paths, out, &lineage, true)
}

/// A membership-log genesis naming `founders`, signed with the key of the vault `paths` admins,
/// the key a configured Mac's lineage check holds a genesis to: for another crate's test that
/// writes a vault's log by hand. This seam is absent from production release builds.
///
/// # Errors
/// [`admin_open_with_candidates`]'s: the vault configuration or admin locker is missing,
/// unreadable or refused by verification, or no reachable server or local cache holds a verified
/// copy of the vault. Or [`Record::found`] fails.
#[cfg(any(test, debug_assertions))]
pub fn test_found(
    paths: &Paths,
    founders: Vec<crate::membership_log::Entrant>,
    authored_ms: u64,
) -> crate::Result<Record> {
    Record::found(&admin_open(paths)?.seed, founders, authored_ms)
}

/// [`admin_sign_genesis`] for the vault whose key is `lineage`, by a build that is `released` or
/// not.
pub(super) fn sign_genesis(
    paths: &Paths,
    out: &Path,
    lineage: &[u8; 32],
    released: bool,
) -> crate::Result<SignedGenesis> {
    if !released {
        return Err(Error::new(
            "Genesis v2 is signed once, with a released aterm: this aterm is a dev build",
        ));
    }
    if std::fs::symlink_metadata(out).is_ok() {
        return Err(Error::new(format!(
            "{} exists: Genesis v2 is signed once, into a new file",
            out.display()
        )));
    }
    let admin = admin_open(paths)?;
    if envelope::signer_public(&admin.seed)? != *lineage {
        return Err(Error::new(
            "Genesis v2 is ALab's, and another key signs this vault",
        ));
    }
    let Some(descriptor) = admin_keys::control_descriptor(&admin.published_doc)? else {
        return Err(Error::new(
            "Genesis v2 lists each Mac's Commons binding, and this vault has no Commons control yet: aterm secrets commons init founds it",
        ));
    };
    let bootstrap = match Bootstrap::observe_until(paths, Instant::now() + Duration::from_secs(30))?
    {
        Availability::Available(bootstrap) => *bootstrap,
        Availability::Unavailable(reason) => {
            return Err(Error::new(format!(
                "reading the Commons bindings needs this Mac's Commons control applied ({}): {}",
                reason.code(),
                reason.explanation()
            )));
        }
    };
    let trust = descriptor.trust();
    if bootstrap.trust() != trust {
        return Err(Error::new(
            "this Mac's Commons root is not the one the vault names",
        ));
    }
    let access = bootstrap.access()?.ok_or_else(|| {
        Error::new(
            "Genesis v2 needs the current Commons roster, and this Mac reads none: its Commons control history is incomplete or conflicting",
        )
    })?;
    let bindings: Vec<_> = access
        .members()
        .iter()
        .map(|member| member.binding().clone())
        .collect();
    let doc = &admin.published_doc;
    let machines = doc
        .machines()
        .into_iter()
        .map(|machine| {
            Ok(Machine {
                recipient: Recipient::parse(&machine.recipient)
                    .map_err(|_| Error::new(format!("{}: bad key", machine.name)))?,
                name: machine.name,
            })
        })
        .collect::<crate::Result<Vec<_>>>()?;
    let roster = super::roster::load_or_seed_swarm_roster_with_log(
        &admin,
        doc,
        &super::roster::log_labels(paths),
    )?;
    let labels = roster
        .members
        .iter()
        .filter_map(|row| {
            let recipient = Recipient::parse(&row.recipient).ok()?;
            Some((recipient, row.label.clone()))
        })
        .collect();
    let policy = SwarmPolicy::from_document(doc)?.unwrap_or_default();
    let reserved = policy.effective_reserved(doc, Some(&roster));
    let next_free = next_free_number(
        &roster,
        doc,
        &reserved,
        &SheetHints::from_document(doc),
        &[],
    );
    let founders = genesis::alab_founders(&machines, &bindings, &labels, next_free)?;
    let members = founders.len();
    let root = Root {
        network: trust.network,
        authority: trust.authority,
    };
    let now_ms = aterm_time::unix_millis().unwrap_or(0);
    let record = Record::genesis(&admin.seed, root, founders, now_ms)?;
    if !fsx::write_new(out, record.bytes(), 0o644)? {
        return Err(Error::new(format!(
            "{} exists: Genesis v2 is signed once, into a new file",
            out.display()
        )));
    }
    let digest = *record.id();

    // F14 (`DESIGN-fluid-swarm` §3.3.6, §4.1): seed initial audience epochs, wraps and encrypted
    // entries into the membership log alongside Genesis v2 (`out` itself remains the single Genesis
    // record whose digest F12b compiles in).
    let mut log = crate::membership_log::Log::new(record)?;
    if let Ok(req) = bootstrap.request() {
        let actor = req.binding().actor();
        let incarnation = *req.binding().incarnation();
        if log.membership().members().contains_key(&actor) {
            bootstrap
                .device()
                .with_seed(|writer_seed| -> crate::Result<()> {
                    let _ = super::log_entries::seed_log_from_document_with_commons_generation(
                        &mut log,
                        doc,
                        &actor,
                        writer_seed,
                        incarnation,
                        now_ms,
                        access.generation().saturating_add(1),
                    )?;
                    Ok(())
                })?;
            let mut store = crate::membership_log::Store::default();
            for rec in log.records() {
                let _ = store.insert(rec.clone())?;
            }
            let wire = store.encode();
            let _ = super::save_records(paths, &wire)?;
            if matches!(admin.config.source, Source::Url(_)) {
                for url in super::ledger_publish::candidate_server_urls(paths, &admin.config, &[]) {
                    let _ = transport::post_records(&url, &wire);
                }
            }
            let _ = super::sync(paths, None);
        }
    }

    Ok(SignedGenesis { digest, members })
}

#[cfg(test)]
mod tests;
