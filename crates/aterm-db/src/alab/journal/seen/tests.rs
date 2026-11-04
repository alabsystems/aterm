// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The journal's readers (design §7): last use, sign-ins and quota from the window, on pure
//! readings and on a scratch Commons lab.

use super::*;
use crate::{
    alab::journal::{
        Presence, Spool,
        event::{Source, Word},
        flush::{self, Flushed},
    },
    table::tests::Lab,
};
use aterm_secrets::commons::roster::Role;

const WEEK: u64 = 7 * 86_400;
const W: u64 = 20_000 / 7;

fn reading(uuid: &str, t: u64, pct: u16, reset: Option<u64>, res: Res) -> Reading {
    Reading {
        uuid: uuid.to_owned(),
        t,
        pct_x10: pct,
        reset,
        res: Some(res),
        src: "statusline".to_owned(),
    }
}

/// A window's usage is the most used of its current readings, and a lower reading made later
/// frees nothing (readings are snapshots); the reset is the most precise reading's, and the figure
/// is credited to the reading that gave it.
/// NEGATIVE CONTROL: with `figure` keyed on `(t, uuid)` instead of `(pct_x10, t, uuid)`, the
/// later lower reading wins: `left: 300 right: 420`.
#[test]
fn a_lower_reading_frees_nothing() {
    let now = 10_000;
    let readings = [
        reading("aa", 9_000, 420, Some(20_000), Res::Exact),
        reading("bb", 9_500, 300, Some(20_030), Res::Hour),
    ];
    let Some(Usage::Current {
        pct_x10,
        reset,
        read,
        by,
        ..
    }) = usage(Window::FiveHour, &readings, now)
    else {
        panic!("current");
    };
    assert_eq!(pct_x10, 420);
    assert_eq!(reset, Some(20_000), "the exact reset");
    assert_eq!(
        (read, by.as_str()),
        (9_000, "aa"),
        "the figure's own reading"
    );
}

/// A group whose reset is a whole window before a newer group's is the window that ended, even
/// while its reset is still inside its tolerance: the new window's figure stands, its reset known.
/// NEGATIVE CONTROL: with `earlier` answering `false` (every current group overlapping), the
/// ended window's 98% outranks the new window's 3%: `left: 980 right: 30`.
#[test]
fn an_ended_window_inside_its_tolerance_does_not_outrank_the_new_one() {
    let now = 100_000;
    let readings = [
        reading("m17", now - 3_600, 980, Some(now - 300), Res::Local),
        reading("m3", now - 60, 30, Some(now + 5 * 3_600), Res::Exact),
    ];
    let Some(Usage::Current {
        pct_x10, reset, by, ..
    }) = usage(Window::FiveHour, &readings, now)
    else {
        panic!("current");
    };
    assert_eq!(pct_x10, 30);
    assert_eq!(reset, Some(now + 5 * 3_600));
    assert_eq!(by, "m3");
}

/// A reading with no reset, made after every named reset passed, is a read since: it is the
/// figure (its reset unknown), never "not read since"; a reset-less reading made before the reset
/// is not.
/// NEGATIVE CONTROL: with the reset-less readings left out of the passed branch (`figure` given
/// an empty list), it answers `ResetUnread`: the `else { panic!("current") }` fires.
#[test]
fn a_reading_without_a_reset_made_since_is_a_read_since() {
    let now = 100_000;
    let no_reset = |uuid: &str, t: u64, pct: u16| Reading {
        reset: None,
        res: None,
        ..reading(uuid, t, pct, None, Res::Exact)
    };
    let readings = [
        reading("aa", now - 7_200, 800, Some(now - 3_600), Res::Exact),
        no_reset("bb", now - 600, 120),
        no_reset("cc", now - 5_000, 900),
    ];
    let Some(Usage::Current {
        pct_x10, reset, by, ..
    }) = usage(Window::FiveHour, &readings, now)
    else {
        panic!("current");
    };
    assert_eq!((pct_x10, reset, by.as_str()), (120, None, "bb"));
}

/// Two current readings whose resets lie further apart than either's resolution name two windows:
/// the reset is shown as unknown, never one of them guessed.
/// NEGATIVE CONTROL: with every current reading counted as one group (`groups` never raised),
/// the reset reads as the exact one: `left: Some(20000) right: None`.
#[test]
fn resets_that_disagree_beyond_their_resolution_are_unknown() {
    let readings = [
        reading("aa", 9_000, 100, Some(20_000), Res::Exact),
        reading("bb", 9_100, 200, Some(27_200), Res::Minute),
    ];
    let Some(Usage::Current { reset, pct_x10, .. }) = usage(Window::FiveHour, &readings, 10_000)
    else {
        panic!("current");
    };
    assert_eq!(reset, None);
    assert_eq!(pct_x10, 200);
}

/// Once every reset read has passed and nothing was read since, the window says so with the
/// reset and the read, never a guessed 0; a reading with no reset at all is shown with its reset
/// unknown.
/// NEGATIVE CONTROL: with `usage` keeping passed readings as current (the `current` filter
/// removed), it answers `Current` with 420: the `else { panic!("reset unread") }` fires.
#[test]
fn a_passed_reset_is_not_read_as_zero() {
    let readings = [reading("aa", 9_000, 420, Some(9_500), Res::Exact)];
    let Some(Usage::ResetUnread { reset, read, .. }) = usage(Window::FiveHour, &readings, 10_000)
    else {
        panic!("reset unread");
    };
    assert_eq!((reset, read), (9_500, 9_000));
    let none = [Reading {
        reset: None,
        res: None,
        ..reading("aa", 9_000, 50, None, Res::Exact)
    }];
    assert!(matches!(
        usage(Window::FiveHour, &none, 10_000),
        Some(Usage::Current {
            reset: None,
            pct_x10: 50,
            ..
        })
    ));
}

fn account(email: &str) -> Pk {
    Pk::new(&keys::account_pk("claude", email).unwrap()).unwrap()
}

fn ev(t: u64, kind: Kind, account: Pk) -> Event {
    Event {
        t,
        kind,
        account: Some(account),
        cli: Word::new("claude").unwrap(),
        sub: None,
        window: None,
        pct_x10: None,
        reset: None,
        res: None,
        until: None,
        plan: None,
        src: Source::CliOutput,
        why: None,
        to: None,
    }
}

/// END TO END on a scratch lab: one Mac's flushed session, sign-in, reading and wall read back as
/// its last use, its sign-in word, the window's usage and the standing wall; a workspace's
/// reading folds under the account as its sub-account; and the heard archive adds only events no
/// window batch holds.
/// NEGATIVE CONTROL: with `Seen::of` not reading `use` rows' sign-ins nor events' (`sign` made a
/// no-op), `signins` is empty: `no sign-in word for ann`.
#[test]
fn the_window_says_who_used_an_account_where_it_is_signed_in_and_its_quota() {
    let lab = Lab::new(&[Role::Writer]);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spool = Spool::at(scratch.path().join("alab"));
    let ann = account("ann@example.test");
    let now = W * WEEK + 3_600;
    let t0 = now - 600;
    spool.append(&ev(t0, Kind::SessionStart, ann)).unwrap();
    spool.append(&ev(t0 + 1, Kind::SignedIn, ann)).unwrap();
    spool
        .append(&Event {
            window: Some(Window::FiveHour),
            pct_x10: Some(420),
            reset: Some(now + 7_200),
            res: Some(Res::Exact),
            src: Source::Statusline,
            ..ev(t0 + 2, Kind::Quota, ann)
        })
        .unwrap();
    spool
        .append(&Event {
            window: Some(Window::Week),
            until: Some(now + 86_400),
            ..ev(t0 + 3, Kind::Wall, ann)
        })
        .unwrap();
    let sub = super::super::event::SubKey::new("0123456789ab").unwrap();
    spool
        .append(&Event {
            window: Some(Window::FiveHour),
            pct_x10: Some(100),
            reset: Some(now + 7_200),
            res: Some(Res::Exact),
            sub: Some(sub),
            src: Source::Rollout,
            ..ev(t0 + 4, Kind::Quota, ann)
        })
        .unwrap();
    let pass = |now| flush::pass(&lab.paths[0], &spool, &Presence::default, false, now).unwrap();
    assert!(matches!(pass(now), Flushed::Wrote { .. }));
    assert!(matches!(pass(now + 60), Flushed::Wrote { .. }));
    let seen = read(&lab.paths[0], Some((&spool, now - WEEK)), now + 120)
        .unwrap()
        .unwrap();
    let (uuid, t) = seen.last_use.get(ann.as_str()).expect("a last use").clone();
    assert_eq!(t, t0);
    let signed = seen
        .signins
        .get(ann.as_str())
        .and_then(|by| by.get(&uuid))
        .expect("no sign-in word for ann");
    assert_eq!((signed.state, signed.t), ("in", t0 + 1));
    let quota = seen.quota(ann.as_str());
    let own = quota
        .iter()
        .find(|q| q.pk == ann.as_str() && q.window == Window::FiveHour)
        .unwrap();
    assert!(matches!(
        own.usage,
        Some(Usage::Current { pct_x10: 420, reset: Some(r), .. }) if r == now + 7_200
    ));
    let walled = quota
        .iter()
        .find(|q| q.pk == ann.as_str() && q.window == Window::Week)
        .unwrap();
    assert_eq!(
        walled.wall.as_ref().map(|w| w.until),
        Some(Some(now + 86_400))
    );
    let sa = keys::subaccount_pk(ann.as_str(), "0123456789ab").unwrap();
    assert!(
        quota
            .iter()
            .any(|q| q.pk == sa && matches!(q.usage, Some(Usage::Current { pct_x10: 100, .. }))),
        "{quota:?}"
    );
    assert!(seen.accounts().contains(ann.as_str()));
    let logged = seen.events.len();
    assert_eq!(logged, 5);
    assert!(seen.events.iter().all(|e| !e.heard));
    // The archive holds nothing the window does not (this Mac's own batch is never archived), so
    // a batch it heard of another Mac is added, and one the window holds is not added twice.
    let mut more = seen.clone();
    let mine = (
        uuid.clone(),
        seen.events[0].n,
        W,
        vec![seen.events[0].event],
    );
    let other = (
        "ab".repeat(16),
        7,
        W,
        vec![ev(t0 - 50, Kind::SignedOut, ann)],
    );
    more.hear(&[mine, other]);
    assert_eq!(more.events.len(), logged + 1);
    assert!(more.events[0].heard, "the oldest, heard");
}

/// A live claims-ring claim is a use (§7 "Who last used account A": live claims naming A): its
/// `since` moves last use forward past an older journal use, it is listed under the account the
/// claims ring keys it by, and a lapsed row or a newer journal use leaves last use alone.
/// NEGATIVE CONTROL: with `claim` leaving `last_use` alone, ann's last use stays the journal's
/// (`left: Some(("aaaa…", 1000)) right: Some(("bbbb…", 1500))`).
#[test]
fn a_live_claim_is_a_use_and_a_lapsed_one_says_nothing() {
    let ann = keys::account_pk("claude", "ann@example.test").unwrap();
    let bob = keys::account_pk("claude", "bob@example.test").unwrap();
    let (a, b, c) = ("a".repeat(32), "b".repeat(32), "c".repeat(32));
    let mut seen = Seen::empty(2_000);
    seen.last_use.insert(ann.clone(), (a, 1_000));
    seen.last_use.insert(bob.clone(), (c.clone(), 1_900));
    let row = |until: i64, email: &str, since: i64| {
        aterm_json::json!({
            "v": 1, "label": "m3", "counter": 1, "until": until,
            "claims": [{"pool": "claude", "email": email, "kind": "lease", "since": since}],
        })
    };
    seen.claim(&[
        ([0xbb; 16], row(2_600, "ann@example.test", 1_500)),
        ([0xbb; 16], row(2_600, "bob@example.test", 1_500)),
        ([0xcc; 16], row(1_999, "ann@example.test", 1_800)),
    ]);
    assert_eq!(seen.last_use.get(&ann), Some(&(b.clone(), 1_500)));
    assert_eq!(
        seen.last_use.get(&bob),
        Some(&(c, 1_900)),
        "a newer use stands"
    );
    assert_eq!(
        seen.claims.get(&ann).map(|held| held
            .iter()
            .map(|h| (h.uuid.clone(), h.kind.clone()))
            .collect::<Vec<_>>()),
        Some(vec![(b, "lease".to_owned())]),
        "the lapsed row is no claim"
    );
}
