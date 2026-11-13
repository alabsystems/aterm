// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm secrets genesis`: a configured vault's admin signs and seeds Genesis v2 once.
//! ALab's admin does so only with a released aterm (`docs/DESIGN-fluid-swarm-2026-10-28.md`
//! §3.3.8, F12), and the release after it compiles the record's digest ([`crate::ALAB_GENESIS`]).
//!
//! The record lists every `[machines]` recipient of the published vault, with the binding
//! Commons' current roster holds for it, under its `aterm-swarm-roster` label. A private vault
//! uses its own pinned signer and current Commons authority. ALab uses `ALAB_SIGNER`, as
//! provenance only: builds check its record by the compiled digest.

use super::*;
use crate::commons::bootstrap::{Availability, Bootstrap};
use crate::membership_log::genesis::{self, Machine};
use crate::membership_log::{Record, Root};

const GENESIS_READ_HANG: Duration = Duration::from_secs(60);

fn genesis_bootstrap_started(paths: &Paths, started: Instant) -> crate::Result<Bootstrap> {
    match Bootstrap::observe_until(paths, started + GENESIS_READ_HANG)? {
        Availability::Available(bootstrap) => Ok(*bootstrap),
        Availability::Unavailable(reason) => Err(Error::new(format!(
            "reading the Commons bindings needs this Mac's Commons control applied ({}): {}",
            reason.code(),
            reason.explanation()
        ))),
    }
}

/// What [`admin_sign_genesis`] wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignedGenesis {
    /// The record's digest: ALab's network id is the one F12b compiles in.
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
    let released = aterm_stamp::installed().is_some_and(|stamp| stamp.release_build);
    sign_genesis(paths, out, &lineage, released)
}

/// The CLI's configured realm: only an explicit private URL gains the private route. A default
/// URL, any known ALab lineage or a live genesis retains the released-admin operation.
pub(crate) fn configured_genesis(paths: &Paths, out: &Path) -> crate::Result<SignedGenesis> {
    let cfg = config(paths)?.ok_or_else(|| Error::new("no vault configured"))?;
    if matches!(cfg.source, Source::Url(_)) && private_realm(paths, &cfg)? {
        seed_vault_genesis(paths, out)
    } else {
        admin_sign_genesis(paths, out)
    }
}

fn private_realm(paths: &Paths, cfg: &Config) -> crate::Result<bool> {
    let reads = Reads::new(paths);
    if matches!(&cfg.source, Source::Url(url) if crate::is_alab_default_url(url))
        || cfg
            .signer
            .is_some_and(|pin| Retired::compiled().lineage(&pin, &reads.lineage(cfg)))
    {
        return Ok(false);
    }
    let (_, mut store) = records_store_selected(&reads, Some(cfg))?;
    let selected = crate::records_storage::selected(&paths.records_file())?;
    let records =
        match fsx::read_private(selected.path(), crate::membership_log::MAX_RECORDS_STREAM)? {
            Some(bytes) => crate::membership_log::decode_records_before_cut(bytes.as_slice())?,
            None => Vec::new(),
        };
    selected.confirm()?;
    let compiled = crate::membership_log::genesis::compiled()?;
    if compiled.is_some_and(|root| {
        store.pinned_network() == Some(root) || records.iter().any(|record| *record.id() == root)
    }) {
        return Ok(false);
    }
    // A broken layout or stream is not a new private realm. Keep strict verification here;
    // the ordinary recovery reader deliberately keeps valid fragments of a damaged file. The
    // one thing read past is a last record cut short, which an append of this Mac's own writer
    // leaves where it died or is read mid-write: the file holds the records before it.
    if store
        .ingest_classified(records)
        .iter()
        .any(|(_, verdict)| matches!(verdict, crate::membership_log::MemberVerdict::Refused(..)))
    {
        return Err(Error::new("membership records contain refused history"));
    }
    Ok(true)
}

fn revalidate_private_genesis(
    paths: &Paths,
    admin: &Admin,
    bootstrap: &Bootstrap,
    access: &crate::commons::roster::Access,
    lock: &StateLock,
) -> crate::Result<()> {
    if config(paths)?.as_ref() != Some(&admin.config) || !private_realm(paths, &admin.config)? {
        return Err(Error::new(
            "the configured genesis realm changed before publication",
        ));
    }
    let current = enrollment::admin_cached_held(paths, lock)?;
    if current.config != admin.config || current.opened_heads != admin.opened_heads {
        return Err(Error::new(
            "the authenticated genesis admin version changed before publication",
        ));
    }
    bootstrap.revalidate_immutable_custody()?;
    let current = bootstrap.access_held(lock)?.ok_or_else(|| {
        Error::new("the genesis Commons roster became unavailable before publication")
    })?;
    if current.head() != access.head()
        || current.basis() != access.basis()
        || current.generation() != access.generation()
        || current.local().binding() != access.local().binding()
    {
        return Err(Error::new(
            "the genesis Commons roster changed before publication",
        ));
    }
    Ok(())
}

/// Sign and seed Genesis v2 into `out` for a non-ALab vault using the signer pinned in `paths`'s
/// `source.toml`.
///
/// # Errors
/// `paths` has no configured signer, its realm is protected or unreadable, its authenticated
/// configuration changes before publication, or [`sign_genesis`] fails.
pub fn seed_vault_genesis(paths: &Paths, out: &Path) -> crate::Result<SignedGenesis> {
    let cfg = config(paths)?.ok_or_else(|| Error::new("no vault configured"))?;
    if !private_realm(paths, &cfg)? {
        return Err(Error::new(
            "the ALab vault's Genesis v2 is signed with admin_sign_genesis",
        ));
    }
    let lineage = cfg
        .signer
        .ok_or_else(|| Error::new("a plain vault has no signer key"))?;
    sign_genesis_in_realm(paths, out, &lineage, true, Some(&cfg))
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
    sign_genesis_in_realm(paths, out, lineage, released, None)
}

fn sign_genesis_in_realm(
    paths: &Paths,
    out: &Path,
    lineage: &[u8; 32],
    released: bool,
    private: Option<&Config>,
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
    if private.is_some_and(|expected| expected != &admin.config) {
        return Err(Error::new(
            "the configured genesis realm changed while opening its admin authority",
        ));
    }
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
    let bootstrap = genesis_bootstrap_started(paths, Instant::now())?;
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
        .filter(|machine| {
            !enrollment::machine_denied_in_doc(doc, &machine.name, &machine.recipient)
        })
        .map(|machine| {
            Ok(Machine {
                recipient: Recipient::parse(&machine.recipient)
                    .map_err(|_| Error::new(format!("{}: bad key", machine.name)))?,
                name: machine.name,
            })
        })
        .collect::<crate::Result<Vec<_>>>()?;
    let roster = super::roster::historical_roster(doc)?;
    let mut labels: BTreeMap<Recipient, String> = roster
        .members
        .iter()
        .filter_map(|row| {
            let recipient = Recipient::parse(&row.recipient).ok()?;
            let row = roster.member_by_recipient(&row.recipient)?;
            (!roster.is_member_denied(row)).then(|| (recipient, row.label.clone()))
        })
        .collect();
    // Genesis still carries protocol display hints. With no historical table, keep a
    // machine's leading local-name hint instead of renaming every seat from m1 upward.
    // No roster entry is created and these names grant no registry identity.
    for machine in &machines {
        if let Some((_, label)) = crate::swarm_roster::leading_m_label(&machine.name) {
            labels.entry(machine.recipient).or_insert(label);
        }
    }
    let founders = genesis::alab_founders(&machines, &bindings, &labels, 1)?;
    let members = founders.len();
    let root = Root {
        network: trust.network,
        authority: trust.authority,
    };
    let now_ms = aterm_time::unix_millis().unwrap_or(0);
    let record = Record::genesis(&admin.seed, root, founders, now_ms)?;
    let digest = *record.id();

    // F14 (`DESIGN-fluid-swarm` §3.3.6, §4.1): seed initial audience epochs, wraps and encrypted
    // entries into the membership log alongside Genesis v2 (`out` itself remains the single Genesis
    // record whose digest F12b compiles in).
    let mut log = crate::membership_log::Log::new(record.clone())?;
    let mut seeded = None;
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
            seeded = Some(store);
        }
    }

    // Network and Commons reads above may block. Bind the private operation to the admin's
    // authenticated source and current local authority while the existing writer lock is held.
    let held = private
        .map(|_| StateLock::take_at(paths, &paths.lock_file()))
        .transpose()?;
    if let Some(lock) = &held {
        revalidate_private_genesis(paths, &admin, &bootstrap, &access, lock)?;
    }
    if !fsx::write_new(out, record.bytes(), 0o644)? {
        return Err(Error::new(format!(
            "{} exists: Genesis v2 is signed once, into a new file",
            out.display()
        )));
    }
    let wire = seeded.as_ref().map(crate::membership_log::Store::encode);
    if let Some(wire) = &wire {
        if held.is_some() {
            let _ = super::save_records_unlocked(paths, wire)?;
        } else {
            let _ = super::save_records(paths, wire)?;
        }
    }
    drop(held);
    if let (Some(store), Some(wire)) = (seeded, wire) {
        if matches!(admin.config.source, Source::Url(_))
            && let Ok(bound) = super::records_bound(paths, &store)
        {
            let urls = super::ledger_publish::candidate_server_urls(paths, &admin.config, &[]);
            let _ = super::fan_out::post_records_to(
                &urls,
                &wire,
                bound,
                super::fan_out::one_exchange(),
            );
        }
        let _ = super::sync(paths, None);
    }

    Ok(SignedGenesis { digest, members })
}

#[cfg(test)]
mod tests;
