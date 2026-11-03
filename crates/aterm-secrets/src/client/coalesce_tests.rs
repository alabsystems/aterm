// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{member_key, two_macs};
use crate::client::log_entries::{self, AuthorCtx};
use crate::client::{Paths, ServersAnswering, load_records, read_identity};
use crate::membership_log::{Body, EpochAudience, Log, Record};
use crate::{age::Recipient, text::Audience};
use std::collections::BTreeSet;

fn epoch_fork(
    base: &Log,
    paths: &Paths,
    audience: &EpochAudience,
    recipients: &[Recipient],
) -> Vec<Record> {
    let identity = read_identity(paths).unwrap().unwrap();
    let opened = base.open_audience_entries(&identity).unwrap().unwrap();
    let generation = opened.max_generation[audience] + 1;
    let (bootstrap, actor, incarnation) = member_key(paths);
    let mut branch = base.clone();
    bootstrap
        .device()
        .with_seed(|seed| -> crate::Result<Vec<Record>> {
            let (epoch, key) = branch.draft_epoch(&actor, 1000, audience.clone(), generation)?;
            let epoch = epoch.sign(seed, incarnation)?;
            let id = *epoch.id();
            branch.insert(epoch.clone())?;
            let mut records = vec![epoch];
            for recipient in recipients {
                let record = branch
                    .draft_wrap(&actor, 1000, id, &key, *recipient)?
                    .sign(seed, incarnation)?;
                branch.insert(record.clone())?;
                records.push(record);
            }
            let put = match audience {
                EpochAudience::Commons => branch.draft_commons_put(
                    &actor,
                    1000,
                    id,
                    &key,
                    opened.commons_control.as_ref().unwrap().as_slice(),
                )?,
                EpochAudience::Everyone => branch.draft_put(
                    &actor,
                    1000,
                    id,
                    &key,
                    &opened.by_audience[&Audience::Everyone]["shared"],
                )?,
                EpochAudience::Hosts(_) => unreachable!(),
            };
            let record = put.sign(seed, incarnation)?;
            branch.insert(record.clone())?;
            records.push(record);
            Ok(records)
        })
        .unwrap()
}

fn merged(base: &Log, branches: &[&[Record]]) -> Log {
    let mut log = base.clone();
    for branch in branches {
        for record in *branch {
            log.insert(record.clone()).unwrap();
        }
    }
    log
}

fn coalesce(
    mut log: Log,
    paths: &Paths,
    active: &BTreeSet<Recipient>,
    reseal: bool,
) -> (Log, Vec<Record>) {
    let identity = read_identity(paths).unwrap().unwrap();
    let opened = log.open_audience_entries(&identity).unwrap().unwrap();
    let (bootstrap, actor, incarnation) = member_key(paths);
    let mut minted = Vec::new();
    bootstrap
        .device()
        .with_seed(|seed| {
            log_entries::coalesce_concurrent_commons_epochs(
                &mut log,
                &opened,
                &AuthorCtx {
                    actor: &actor,
                    writer_seed: seed,
                    incarnation,
                    authored_ms: 2000,
                },
                active,
                &mut minted,
                reseal,
            )
        })
        .unwrap();
    (log, minted)
}

fn partial_holder(audience: EpochAudience) {
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[(Audience::Everyone, "shared", b"value")]);
    let base = load_records(&macs.m22).unwrap().log().unwrap().clone();
    let partial = read_identity(&macs.m22).unwrap().unwrap();
    let full = read_identity(&macs.m7).unwrap().unwrap();
    let active = BTreeSet::from([partial.recipient(), full.recipient()]);
    let first = epoch_fork(
        &base,
        &macs.m22,
        &audience,
        &[partial.recipient(), full.recipient()],
    );
    let second = epoch_fork(&base, &macs.m7, &audience, &[full.recipient()]);
    let before = merged(&base, &[&first, &second]);
    let partial_view = before.open_audience_entries(&partial).unwrap().unwrap();
    assert!(!partial_view.fully_unwrapped_audiences.contains(&audience));
    assert!(
        before
            .open_audience_entries(&full)
            .unwrap()
            .unwrap()
            .fully_unwrapped_audiences
            .contains(&audience)
    );
    let partial_due = before.needs_commons_reconcile(&partial.recipient(), &active, true);
    let full_due = before.needs_commons_reconcile(&full.recipient(), &active, true);
    let outcomes: Vec<_> = [false, true]
        .into_iter()
        .map(|reseal| {
            (
                coalesce(before.clone(), &macs.m22, &active, reseal),
                coalesce(before.clone(), &macs.m7, &active, reseal),
            )
        })
        .collect();
    macs.server.stop();

    assert!(
        !partial_due,
        "a partial holder is not scheduled for a no-op"
    );
    assert!(full_due);
    for ((unchanged, partial_minted), (completed, full_minted)) in outcomes {
        assert!(
            partial_minted.is_empty(),
            "a partial holder changed the epoch"
        );
        assert_eq!(unchanged.heads(), before.heads());
        assert!(!full_minted.is_empty());
        let opened = completed.open_audience_entries(&full).unwrap().unwrap();
        assert_eq!(opened.maximal_epochs[&audience].len(), 1);
        assert!(opened.commons_control.is_some());
        if audience == EpochAudience::Everyone {
            assert_eq!(
                opened.by_audience[&Audience::Everyone]["shared"]
                    .value
                    .as_slice(),
                b"value"
            );
        }
        assert!(completed.audience_left_out_recipient(&audience, &partial.recipient()));
        assert!(!completed.audience_left_out_recipient(&audience, &full.recipient()));
        assert!(!completed.needs_commons_reconcile(&full.recipient(), &active, true));
    }
}

#[test]
fn a_partial_commons_holder_cannot_dominate_an_epoch_an_active_member_still_opens() {
    partial_holder(EpochAudience::Commons);
}

#[test]
fn a_partial_everyone_holder_cannot_dominate_an_epoch_an_active_member_still_opens() {
    partial_holder(EpochAudience::Everyone);
}

fn delayed_wraps(audience: EpochAudience) {
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[(Audience::Everyone, "shared", b"value")]);
    let base = load_records(&macs.m22).unwrap().log().unwrap().clone();
    let observer = read_identity(&macs.m22).unwrap().unwrap();
    let other = read_identity(&macs.m7).unwrap().unwrap();
    let first = epoch_fork(
        &base,
        &macs.m22,
        &audience,
        &[observer.recipient(), other.recipient()],
    );
    let second = epoch_fork(
        &base,
        &macs.m7,
        &audience,
        &[other.recipient(), observer.recipient()],
    );
    let active = BTreeSet::from([observer.recipient()]);
    let outcomes: Vec<_> = [1, 2]
        .into_iter()
        .map(|prefix| {
            let before = merged(&base, &[&first, &second[..prefix]]);
            let due = before.needs_commons_reconcile(&observer.recipient(), &active, true);
            let partial = coalesce(before, &macs.m22, &active, false);
            let complete = coalesce(merged(&base, &[&first, &second]), &macs.m22, &active, false);
            (due, partial, complete)
        })
        .collect();
    macs.server.stop();

    for (due, (_, early), (completed, minted)) in outcomes {
        assert!(!due, "an unopened epoch is not scheduled for a no-op");
        assert!(
            early.is_empty(),
            "an unopened epoch was treated as abandoned"
        );
        assert!(minted.iter().any(|record| matches!(
            record.body(),
            Body::Epoch { audience: actual, .. } if actual == &audience
        )));
        assert!(completed.open_commons_control(&observer).unwrap().is_some());
        if audience == EpochAudience::Everyone {
            let opened = completed.open_audience_entries(&observer).unwrap().unwrap();
            assert_eq!(
                opened.by_audience[&Audience::Everyone]["shared"]
                    .value
                    .as_slice(),
                b"value"
            );
        }
    }
}

#[test]
fn an_unopened_commons_epoch_waits_for_its_later_wraps_even_when_none_are_active_yet() {
    delayed_wraps(EpochAudience::Commons);
}

#[test]
fn an_unopened_everyone_epoch_waits_for_its_later_wraps_even_when_none_are_active_yet() {
    delayed_wraps(EpochAudience::Everyone);
}
