// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{member_key, two_macs};
use crate::client::log_entries::{self, AuthorCtx};
use crate::client::{Paths, ServersAnswering, load_records, read_identity};
use crate::membership_log::{Body, EpochAudience, Log, MemberId, Record};
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

fn coalesce(mut log: Log, paths: &Paths, active: &BTreeSet<Recipient>) -> (Log, Vec<Record>) {
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
                &[EpochAudience::Everyone, EpochAudience::Commons],
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
    let (_, partial_actor, _) = member_key(&macs.m22);
    let (_, full_actor, _) = member_key(&macs.m7);
    let carriers = carriers_of(&before, &audience);
    let outcomes = [(
        coalesce(before.clone(), &macs.m22, &active),
        coalesce(before.clone(), &macs.m7, &active),
    )];
    macs.server.stop();

    assert!(
        !carriers.contains(&partial_actor),
        "a partial holder is not ranked for a no-op"
    );
    assert!(carriers.contains(&full_actor));
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
        assert!(
            completed
                .rekey_dues()
                .iter()
                .all(|due| due.audience != audience)
        );
    }
}

/// The members `log` ranks for a re-key of `audience` it has due ([`Log::rekey_dues`]).
fn carriers_of(log: &Log, audience: &EpochAudience) -> BTreeSet<MemberId> {
    log.rekey_dues()
        .into_iter()
        .find(|due| due.audience == *audience)
        .map(|due| due.carriers)
        .unwrap_or_default()
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
    let (_, observer_actor, _) = member_key(&macs.m22);
    let outcomes: Vec<_> = [1, 2]
        .into_iter()
        .map(|prefix| {
            let before = merged(&base, &[&first, &second[..prefix]]);
            let ranked = carriers_of(&before, &audience).contains(&observer_actor);
            let partial = coalesce(before, &macs.m22, &active);
            let whole = merged(&base, &[&first, &second]);
            let twins_due = whole
                .rekey_dues()
                .iter()
                .any(|due| due.audience == audience);
            let twins = coalesce(whole.clone(), &macs.m22, &active).1;
            crate::membership_log::rekey::TWINS_DUE.with(|due| due.set(true));
            let complete = coalesce(whole, &macs.m22, &active);
            crate::membership_log::rekey::TWINS_DUE.with(|due| due.set(false));
            (ranked, partial, twins_due, twins, complete)
        })
        .collect();
    macs.server.stop();

    for (ranked, (_, early), twins_due, twins, (completed, minted)) in outcomes {
        assert!(!ranked, "an unopened epoch is not ranked for a no-op");
        assert!(
            early.is_empty(),
            "an unopened epoch was treated as abandoned"
        );
        // Once its wraps arrive the two epochs wrap one recipient set: secure twins, not due.
        assert!(!twins_due, "secure twins are not due a coalesce");
        assert!(twins.is_empty(), "secure twins are not coalesced");
        // CONTROL: where twins were due, as every concurrent pair once was, the complete epoch is
        // coalesced, so it is the twins rule that holds the mint back.
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
