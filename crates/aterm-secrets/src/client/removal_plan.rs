// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Prepare the complete local security transaction before a revoke becomes
//! durable. Ordinary document edits keep their existing publish-before-retain
//! ordering; only the explicit revoke path commits its retirement and cascade
//! together before the envelope is sent.

use super::*;
use crate::membership_log::{Record, RecordCounts, Store};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Footprint {
    /// Complete records, by the budget each counts against.
    pub(super) complete: RecordCounts,
    pub(super) stored_records: usize,
    pub(super) encoded_bytes: usize,
}

impl Footprint {
    fn of(store: &Store) -> Self {
        Self {
            complete: store
                .log()
                .map_or_else(RecordCounts::default, |log| log.counts()),
            stored_records: store.len(),
            encoded_bytes: store.encoded_len(),
        }
    }
}

/// Exact only after every planned record was retained in the in-memory store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Cost {
    pub(super) before: Footprint,
    pub(super) after: Footprint,
}

pub(super) struct Prepared {
    // Local signing stays serialized through final validation and commit.
    _author: log_entries::RecordsAuthorLock,
    version: [u8; 32],
    inventory: BTreeSet<[u8; 32]>,
    recipient: Recipient,
    actor: Option<crate::membership_log::MemberId>,
    recipients: Vec<Recipient>,
    records: Vec<Record>,
    pub(super) drafted: Option<log_entries::Drafted>,
    pub(super) cost: Cost,
    retirement: Option<retirement::Retirement>,
    bootstrap: Option<crate::commons::bootstrap::Bootstrap>,
    legacy: bool,
}

pub(super) enum Committed {
    /// Peer ingress changed the snapshot. Discard every prepared byte and replan.
    Changed,
    Done(Option<retirement::Retirement>),
}

/// Keep every one of `records` in `store` as a complete record, or refuse the batch whole. The
/// planner drafted each into a log of the same records file under the same budgets, whose
/// `Log::insert` refuses one past any of them as `MembershipCapacity`, naming the budget
/// (`plan_error` keeps that text), so a batch that reaches this fits them; what this still
/// refuses is a record the store does not keep complete.
fn retain_complete(store: &mut Store, records: &[Record]) -> crate::Result<()> {
    store.ingest_records(records.to_vec())?;
    if records.iter().any(|record| !store.holds(record.id())) {
        return Err(Error::of_kind(
            crate::ErrorKind::MembershipCapacity,
            "the complete removal batch could not be retained",
        ));
    }
    Ok(())
}

/// A stopped bounded planner proves only this lower bound, never permission to
/// proceed. Waiting records are excluded because ordinary insertion may evict
/// them; no capacity constant or wire-size estimate is duplicated here.
fn plan_error(error: Error, before: &Store, prefix: &[Record]) -> Error {
    if error.kind() != crate::ErrorKind::MembershipCapacity {
        return error;
    }
    let complete = before.log().map_or(0, |log| log.held_count());
    let bytes: usize = before
        .log()
        .into_iter()
        .flat_map(|log| log.records())
        .map(|record| record.bytes().len())
        .sum();
    let additional: usize = prefix.iter().map(|record| record.bytes().len()).sum();
    Error::of_kind(
        crate::ErrorKind::MembershipCapacity,
        format!(
            "removal cannot fit: at least {} complete records and {} encoded bytes are needed; \
             the bounded plan stopped before its full cost was known ({error})",
            complete.saturating_add(prefix.len()).saturating_add(1),
            bytes.saturating_add(additional).saturating_add(1),
        ),
    )
}

pub(super) fn prepare(
    paths: &Paths,
    admin: &Admin,
    doc: &Document,
    identity: &Identity,
    recipients: &[Recipient],
    deadline: Option<Instant>,
) -> crate::Result<Prepared> {
    publication_current(deadline)?;
    let author = match deadline {
        Some(deadline) => log_entries::RecordsAuthorLock::try_take_until(paths, deadline)?
            .ok_or_else(|| {
                Error::of_kind(
                    crate::ErrorKind::TimedOut,
                    "removal planning deadline expired",
                )
            })?,
        None => log_entries::RecordsAuthorLock::take(paths)?,
    };
    StateLock::narrow_owned(paths)?;
    // Signing may inspect several thousand retained records. Hold a shared
    // observation, never the exclusive state lock, throughout that bounded work.
    let observation = StateLock::observe(paths)?
        .ok_or_else(|| Error::of_kind(crate::ErrorKind::VaultBusy, "vault state is busy"))?;
    let state = load_state(paths)?;
    let different_opened_version = state.seq == admin.seq
        && state
            .digest
            .is_some_and(|digest| admin.opened_heads.first() != Some(&digest));
    if config(paths)?.as_ref() != Some(&admin.config)
        || state.seq > admin.seq
        || different_opened_version
        || (state.membership == Stored::In
            && !source_bound(&admin.config, &state)
            && !log_only_source_bound(&admin.config, &state))
    {
        return Err(Error::of_kind(
            crate::ErrorKind::VaultChanged,
            "the vault changed before removal planning; reopen it",
        ));
    }
    let version = service_health_version(paths)?;
    let before = load_records(paths)?;
    let inventory = before.inventory();
    let mut candidate = before.clone();
    let mut records = Vec::new();
    let mut retirement = None;
    let mut custody = None;
    let mut legacy = true;
    if let Some(descriptor) = admin_keys::control_descriptor(&admin.published_doc)? {
        for recipient in recipients {
            retirement::non_admin_target(admin, *recipient)?;
        }
        let bootstrap = retirement::removal_custody(paths, admin)?;
        if bootstrap.trust() != descriptor.trust() {
            return Err(Error::new(
                "cached Commons root differs from the admin snapshot",
            ));
        }
        if let Some((log, head)) =
            log_entries::trusted_commons_log_from_store(paths, &candidate, bootstrap.trust())?
        {
            if !matches!(
                admin.log_base,
                log_entries::LogBase::Covers | log_entries::LogBase::Unstamped
            ) {
                return Err(Error::new(
                    "the local log does not cover the opened vault; sync it before revoking",
                ));
            }
            legacy = false;
            let targets = recipients.iter().copied().collect();
            let planned = admin
                .plan_commons_members_in_log(&bootstrap, &log, head, &targets, &observation)
                .map_err(|error| plan_error(error, &before, &records))?
                .ok_or_else(|| Error::new("removal names an unknown Commons recipient"))?;
            records = planned.records;
            retirement = Some(planned.retirement);
            retain_complete(&mut candidate, &records)
                .map_err(|error| plan_error(error, &before, &[]))?;
        }
        custody = Some(bootstrap);
    }
    let draft = match admin.log_base {
        log_entries::LogBase::Covers | log_entries::LogBase::Unstamped => {
            let trusted = log_entries::trusted_seeded_store(
                paths,
                &admin.config,
                &envelope::signer_public(&admin.seed)?,
                &admin.handoff,
            )?;
            if trusted.is_some() {
                log_entries::draft_document_diff_in_store(
                    paths,
                    &candidate,
                    identity,
                    doc,
                    log_entries::Base {
                        doc: (admin.log_base == log_entries::LogBase::Covers)
                            .then_some(&admin.published_doc),
                        resolve: Some(&admin.resolve),
                        readmit: None,
                    },
                    deadline,
                )
                .map_err(|error| plan_error(error, &before, &records))?
            } else {
                None
            }
        }
        log_entries::LogBase::Behind | log_entries::LogBase::NoLog => None,
    };
    let has_draft = draft.is_some();
    if let Some(draft) = draft {
        retain_complete(&mut candidate, &draft.records)
            .map_err(|error| plan_error(error, &before, &records))?;
        records.extend(draft.records);
    }
    if !legacy {
        let descriptor = admin_keys::control_descriptor(&admin.published_doc)?
            .ok_or_else(|| Error::new("the removal lost its Commons descriptor"))?;
        let (log, head) =
            log_entries::trusted_commons_log_from_store(paths, &candidate, descriptor.trust())?
                .ok_or_else(|| Error::new("the prepared removal lost its trusted Commons log"))?;
        if recipients.iter().any(|recipient| {
            log.membership()
                .members()
                .values()
                .any(|member| member.binding.recipient() == *recipient)
                || log.wraps_maximal_audience_to(
                    &crate::membership_log::EpochAudience::Commons,
                    recipient,
                )
                || log.wraps_maximal_audience_to(
                    &crate::membership_log::EpochAudience::Everyone,
                    recipient,
                )
        }) {
            return Err(Error::new(
                "the prepared removal did not retain its recipient restriction",
            ));
        }
        if let Some(retirement) = &mut retirement {
            retirement.head = head;
        }
    }
    let drafted = (!legacy || has_draft)
        .then(|| candidate.log())
        .flatten()
        .map(|log| log_entries::Drafted::from_log(records.clone(), log));
    let cost = Cost {
        before: Footprint::of(&before),
        after: Footprint::of(&candidate),
    };
    let actor = records.first().map(|record| *record.actor());
    drop(observation);
    Ok(Prepared {
        _author: author,
        version,
        inventory,
        recipient: identity.recipient(),
        actor,
        recipients: recipients.to_vec(),
        records,
        drafted,
        cost,
        retirement,
        bootstrap: custody,
        legacy,
    })
}

impl Prepared {
    /// Called only after all local envelope sealing/validation succeeded. The
    /// final exclusive phase validates the same source and inventory, then keeps
    /// every record atomically. No network operation runs while either lock lives.
    pub(super) fn commit(
        self,
        paths: &Paths,
        admin: &Admin,
        doc: &Document,
        deadline: Option<Instant>,
    ) -> crate::Result<Committed> {
        let state = configured_admin_until(paths, &admin.config, deadline)?;
        if service_health_version(paths)? != self.version
            || read_identity(paths)?.map(|identity| identity.recipient()) != Some(self.recipient)
        {
            return Ok(Committed::Changed);
        }
        let mut current = load_records(paths)?;
        if current.inventory() != self.inventory || Footprint::of(&current) != self.cost.before {
            return Ok(Committed::Changed);
        }
        if let Some(bootstrap) = &self.bootstrap {
            bootstrap.revalidate_immutable_custody()?;
        }
        if let Some(actor) = self.actor {
            let Some(log) = current.log() else {
                return Ok(Committed::Changed);
            };
            let crate::membership_log::Body::Genesis { root, .. } = log.genesis().body() else {
                return Ok(Committed::Changed);
            };
            let device = match deadline {
                Some(deadline) => {
                    crate::commons::bootstrap::Device::open_existing_until(paths, deadline)?
                }
                None => crate::commons::bootstrap::Device::open_existing(paths)?,
            };
            if device
                .request(root.network, root.authority)?
                .binding()
                .actor()
                != actor
            {
                return Ok(Committed::Changed);
            }
        }
        let retirement = if self.legacy {
            retirement::retire_cached_held(
                paths,
                admin,
                doc,
                &self.recipients,
                &state,
                self.bootstrap.as_ref(),
            )?
        } else {
            self.retirement
        };
        if !self.records.is_empty() {
            let wire = crate::membership_log::encode_records(&self.records);
            log_entries::commit_authored_records(paths, &mut current, &wire, &self.records)
                .map_err(|error| Error::of_kind(error.kind(), format!(
                    "removal commit is unconfirmed; retry the same revoke before assuming it failed: {error}"
                )))?;
            aterm_durable::durable_dir(&paths.dir).map_err(|error| Error::new(format!(
                "removal durability is unconfirmed; retry the same revoke before assuming it failed: {error}"
            )))?;
        }
        debug_assert_eq!(Footprint::of(&current), self.cost.after);
        Ok(Committed::Done(retirement))
    }
}
