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
    }) = usage(Window::FiveHour, &readings, None, now)
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
    }) = usage(Window::FiveHour, &readings, None, now)
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
    }) = usage(Window::FiveHour, &readings, None, now)
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
    let Some(Usage::Current { reset, pct_x10, .. }) =
        usage(Window::FiveHour, &readings, None, 10_000)
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
    let Some(Usage::ResetUnread { reset, read, .. }) =
        usage(Window::FiveHour, &readings, None, 10_000)
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
        usage(Window::FiveHour, &none, None, 10_000),
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

/// A week's reading of `account` at `t`, `pct` tenths used, its reset `reset`.
fn week_read(t: u64, account: Pk, pct: u16, reset: u64) -> Event {
    Event {
        window: Some(Window::Week),
        pct_x10: Some(pct),
        reset: Some(reset),
        res: Some(Res::Exact),
        src: Source::Statusline,
        ..ev(t, Kind::Quota, account)
    }
}

/// The week `seen` folds of `account`: its figure and the Mac credited with it.
fn week_of(seen: &Seen, account: Pk) -> Option<(u16, String)> {
    seen.quota(account.as_str())
        .into_iter()
        .find(|q| q.pk == account.as_str() && q.window == Window::Week)
        .and_then(|q| match q.usage? {
            Usage::Current { pct_x10, by, .. } => Some((pct_x10, by)),
            Usage::ResetUnread { .. } => None,
        })
}

/// A `reset-used` that refilled `window` of `account` at `t`.
fn refill(t: u64, account: Pk, window: Window) -> Event {
    Event {
        window: Some(window),
        ..ev(t, Kind::ResetUsed, account)
    }
}

/// A wall of `account`'s week seen at `t`, standing until `until`.
fn week_wall(t: u64, account: Pk, until: u64) -> Event {
    Event {
        window: Some(Window::Week),
        until: Some(until),
        ..ev(t, Kind::Wall, account)
    }
}

/// Whether `seen` holds a standing wall of `account`'s `window`.
fn walled(seen: &Seen, account: Pk, window: Window) -> bool {
    seen.quota(account.as_str())
        .iter()
        .any(|q| q.window == window && q.wall.is_some())
}

/// A REFILL CUTS THE WINDOW'S OLDER READINGS: 90% read at t100, a `reset-used` of the week at t200
/// and 10% at t300 under the same reset is 10%, though a lower reading frees nothing within one
/// window; a reading made in the refill's second stands.
/// NEGATIVE CONTROL: without the `reset-used` the 90% stands (readings are snapshots), and with
/// `usage` given no cut (`None` for `cut`), the refilled window reads 90%: `left: Some(900)
/// right: Some(100)`.
#[test]
fn a_reset_used_cuts_the_readings_before_it() {
    const MAC: &str = "000000000000000000000000000000aa";
    let ann = account("ann@example.test");
    let reset = 7 * 86_400;
    let read = |events: &[Event]| {
        let events: Vec<(String, Event)> = events.iter().map(|e| (MAC.to_owned(), *e)).collect();
        week_of(&Seen::of_events(&events, 400), ann).map(|(pct, _)| pct)
    };
    let before = week_read(100, ann, 900, reset);
    let after = week_read(300, ann, 100, reset);
    assert_eq!(
        read(&[before, after]),
        Some(900),
        "CONTROL: a lower reading frees nothing"
    );
    let refill = refill(200, ann, Window::Week);
    assert_eq!(read(&[before, refill, after]), Some(100));
    assert_eq!(
        read(&[before, after, refill]),
        Some(100),
        "the batch's order is not the cut's"
    );
    let same_second = week_read(200, ann, 50, reset);
    assert_eq!(read(&[before, refill, same_second]), Some(50));
    assert_eq!(
        read(&[before, refill]),
        None,
        "nothing read since the refill"
    );
}

/// ONE MAC'S REFILL CUTS EVERY MAC'S OLDER READINGS OF THE WINDOW: a `reset-used` of the week m3
/// logged cuts m17's week reading made before it, and m17's reading made after it stands; a wall
/// of the week m17 saw before the refill no longer stands; another account's readings and wall
/// are left.
/// NEGATIVE CONTROL: with the cut keyed by (account, Mac), m17's 95% stands: `left: Some((950,
/// "…bb")) right: Some((200, "…bb"))`; with `quota` not cutting walls, ann's wall stands.
#[test]
fn one_macs_reset_used_cuts_another_macs_older_readings() {
    let (m3, m17) = ("a".repeat(32), "b".repeat(32));
    let (ann, bob) = (account("ann@example.test"), account("bob@example.test"));
    let reset = 7 * 86_400;
    let seen = Seen::of_events(
        &[
            (m17.clone(), week_read(100, ann, 950, reset)),
            (m17.clone(), week_wall(150, ann, reset)),
            (m17.clone(), week_read(100, bob, 700, reset)),
            (m17.clone(), week_wall(150, bob, reset)),
            (m3.clone(), refill(200, ann, Window::Week)),
            (m17.clone(), week_read(300, ann, 200, reset)),
        ],
        400,
    );
    assert_eq!(week_of(&seen, ann), Some((200, m17.clone())));
    assert_eq!(
        week_of(&seen, bob),
        Some((700, m17)),
        "another account's week"
    );
    assert!(
        !walled(&seen, ann, Window::Week),
        "a wall seen before the refill no longer stands"
    );
    assert!(
        walled(&seen, bob, Window::Week),
        "another account's wall stands"
    );
}

/// A RESET SPENT WITH THE LIMIT STILL STANDING CUTS NOTHING: m13 walls ann's week at t100 and
/// logs a `reset-used` naming no window at t200 (the harness writes one for a reset the vendor
/// says was spent while the walled limit still stands, and for no other); the wall and m13's 95%
/// read before it still stand.
/// NEGATIVE CONTROL: with a `reset-used` of no window read as cutting every window of its key (as
/// it once was), the wall and the reading are gone: `wall still stands` fails.
#[test]
fn a_reset_spent_with_the_limit_still_standing_cuts_nothing() {
    let m13 = "d".repeat(32);
    let ann = account("ann@example.test");
    let reset = 7 * 86_400;
    let seen = Seen::of_events(
        &[
            (m13.clone(), week_read(90, ann, 950, reset)),
            (m13.clone(), week_wall(100, ann, reset)),
            (m13.clone(), ev(200, Kind::ResetUsed, ann)),
        ],
        400,
    );
    assert!(walled(&seen, ann, Window::Week), "the wall still stands");
    assert_eq!(week_of(&seen, ann), Some((950, m13)), "the reading stands");
}

/// A FIVE-HOUR REFILL LEAVES THE WEEK: m3's `reset-used` of the five-hour window cuts a peer's
/// five-hour reading made before it; m13's 95% week reading and its week wall, both before it,
/// stand.
/// NEGATIVE CONTROL: with the cut keyed by account alone (every window of the key), the week's
/// reading is gone: `left: None right: Some((950, …))`.
#[test]
fn a_five_hour_refill_leaves_the_week() {
    let (m3, m13) = ("a".repeat(32), "d".repeat(32));
    let ann = account("ann@example.test");
    let reset = 7 * 86_400;
    let five = Event {
        window: Some(Window::FiveHour),
        reset: Some(18_000),
        ..week_read(100, ann, 800, reset)
    };
    let seen = Seen::of_events(
        &[
            (m13.clone(), week_read(100, ann, 950, reset)),
            (m13.clone(), week_wall(150, ann, reset)),
            (m13.clone(), five),
            (m3.clone(), refill(200, ann, Window::FiveHour)),
        ],
        400,
    );
    assert_eq!(week_of(&seen, ann), Some((950, m13)), "the week's reading");
    assert!(walled(&seen, ann, Window::Week), "the week's wall");
    assert!(
        !seen
            .quota(ann.as_str())
            .iter()
            .any(|q| q.window == Window::FiveHour && q.usage.is_some()),
        "the five-hour reading made before the refill is cut"
    );
}

/// A REFILL DATED BY A CLOCK AHEAD CUTS NO MORE THAN NOW ALLOWS: a `reset-used` m17 dated two
/// hours past the read is not believed (m3's 95% read before the read stands); one dated ten
/// minutes past it holds from the read's now (the 95% made before now is cut, a reading made at
/// now stands).
/// NEGATIVE CONTROL: with the cut stored as the event's raw time, the reading at now is cut too:
/// `left: None right: Some(30)`; with no bound ahead, the two-hour refill cuts the 95%.
#[test]
fn a_refill_dated_ahead_of_the_read_cuts_no_more_than_now() {
    let (m3, m17) = ("a".repeat(32), "b".repeat(32));
    let ann = account("ann@example.test");
    let (now, reset) = (10_000, 7 * 86_400);
    let read = |refill_at: u64, events: &[(String, Event)]| {
        let mut events = events.to_vec();
        events.push((m17.clone(), refill(refill_at, ann, Window::Week)));
        week_of(&Seen::of_events(&events, now), ann).map(|(pct, _)| pct)
    };
    let before = (m3.clone(), week_read(now - 100, ann, 950, reset));
    assert_eq!(
        read(now + 2 * 3_600, std::slice::from_ref(&before)),
        Some(950),
        "a refill two hours ahead is not believed"
    );
    assert_eq!(
        read(now + 600, std::slice::from_ref(&before)),
        None,
        "a refill ten minutes ahead holds from now"
    );
    assert_eq!(
        read(
            now + 600,
            &[before.clone(), (m3.clone(), week_read(now, ann, 30, reset))]
        ),
        Some(30),
        "a reading made at now stands"
    );
}

/// A PEER'S READING DATED JUST AFTER A REFILL COUNTS ONLY UNTIL A LATER SURE ONE: m17's clock
/// runs ten minutes ahead, and its 95% read before m3's refill is dated five minutes after it.
/// With nothing read since it counts (it may be of the refilled window); m3's own 3% read before
/// it does not outrank it; m3's 4% dated after it (the refilling Mac's clock, so surely after the
/// refill) shows the window: the 95% is from before the refill. A peer's reading a skew bound
/// past the refill is sure too and counts as most used as any.
/// NEGATIVE CONTROL: with every reading at or after the cut counted alike, m17's 95% stands over
/// m3's 4%: `left: Some((950, "…bb")) right: Some((40, "…aa"))`.
#[test]
fn a_peers_reading_just_after_a_refill_counts_until_a_later_sure_one() {
    let (m3, m17) = ("a".repeat(32), "b".repeat(32));
    let ann = account("ann@example.test");
    let (cut, reset) = (12 * 3_600, 7 * 86_400);
    let read = |events: &[(String, Event)]| {
        let mut events = events.to_vec();
        events.push((m3.clone(), refill(cut, ann, Window::Week)));
        week_of(&Seen::of_events(&events, cut + 3 * 3_600), ann)
    };
    let stale = (m17.clone(), week_read(cut + 300, ann, 950, reset));
    assert_eq!(
        read(std::slice::from_ref(&stale)),
        Some((950, m17.clone())),
        "with nothing sure read since, it counts"
    );
    let early = (m3.clone(), week_read(cut + 60, ann, 30, reset));
    assert_eq!(
        read(&[stale.clone(), early.clone()]),
        Some((950, m17.clone())),
        "a sure reading made before it does not outrank it"
    );
    let later = (m3.clone(), week_read(cut + 360, ann, 40, reset));
    assert_eq!(
        read(&[stale.clone(), early.clone(), later.clone()]),
        Some((40, m3.clone())),
        "a later sure reading shows the window"
    );
    let past_skew = (m17.clone(), week_read(cut + 900, ann, 60, reset));
    assert_eq!(
        read(&[stale, early, later, past_skew]),
        Some((60, m17)),
        "a peer's reading a skew bound past the refill is sure"
    );
}

/// A WORKSPACE'S REFILL CUTS ITS SUB-ACCOUNT'S WINDOW: a `reset-used` noted with the workspace's
/// `sub`, as its readings and walls are, cuts the sub-account's week reading and wall made before
/// it; one noted of the account alone leaves the workspace's.
/// NEGATIVE CONTROL: with the fold keying a `reset-used` by its account whatever its `sub`, the
/// workspace's 95% stands: `left: Some(950) right: None`.
#[test]
fn a_workspaces_refill_cuts_its_sub_accounts_window() {
    let m13 = "d".repeat(32);
    let ann = account("ann@example.test");
    let sub = Some(Word::new("0123456789ab").unwrap());
    let sa = keys::subaccount_pk(ann.as_str(), "0123456789ab").unwrap();
    let reset = 7 * 86_400;
    let workspace = |event: Event| (m13.clone(), Event { sub, ..event });
    let held = |events: &[(String, Event)]| {
        let seen = Seen::of_events(events, 400);
        let week = seen
            .quota(ann.as_str())
            .into_iter()
            .find(|q| q.pk == sa && q.window == Window::Week);
        let pct = week.as_ref().and_then(|q| match q.usage.as_ref()? {
            Usage::Current { pct_x10, .. } => Some(*pct_x10),
            Usage::ResetUnread { .. } => None,
        });
        (pct, week.is_some_and(|q| q.wall.is_some()))
    };
    let before = [
        workspace(week_read(100, ann, 950, reset)),
        workspace(week_wall(150, ann, reset)),
    ];
    let mut cut = before.to_vec();
    cut.push(workspace(refill(200, ann, Window::Week)));
    assert_eq!(held(&cut), (None, false), "the workspace's window is cut");
    let mut account_only = before.to_vec();
    account_only.push((m13.clone(), refill(200, ann, Window::Week)));
    assert_eq!(
        held(&account_only),
        (Some(950), true),
        "the account's refill is not the workspace's"
    );
}

fn ev(t: u64, kind: Kind, account: Pk) -> Event {
    Event {
        t,
        kind,
        account: Some(account),
        cli: Word::new("claude").unwrap(),
        sub: None,
        window: None,
        model: None,
        pct_x10: None,
        reset: None,
        res: None,
        until: None,
        plan: None,
        src: Source::CliOutput,
        why: None,
        to: None,
        tab: None,
        harness: None,
        op: None,
        via: None,
        program: None,
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
    assert_eq!(
        seen.me,
        [uuid.clone()].into(),
        "the Mac that wrote the batch reads it as its own"
    );
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

/// A sighting states no sign-in word to the window's reader either, so one Mac's `ended` is not
/// read as `in` by a later `seen`; a login that ended does state its word.
/// NEGATIVE CONTROL: with `Kind::Seen` giving `in` in `Event::signin`, `` left: Some("in")
/// right: None ``.
#[test]
fn a_sighting_states_no_sign_in_word() {
    let ann = account("ann@example.test");
    assert_eq!(ev(1, Kind::Seen, ann).signin(), None);
    assert_eq!(ev(1, Kind::LoginEnded, ann).signin(), Some("ended"));
    assert_eq!(ev(1, Kind::SignedIn, ann).signin(), Some("in"));
}

/// What a peer reads of a Mac's published sign-in words: `auth` saying `ok` (a login that reads
/// working again, as a Mac states its standing word) is `in` over an older `signed-out`; `auth`
/// saying `unseen` withdraws the Mac's word, so the account has none there, and neither an older
/// event read after it nor that Mac's older `use` row restores one; a later sign-in does. An
/// `auth` with any other reason states nothing.
/// NEGATIVE CONTROLS: with the `Kind::Auth` arm of `Event::signin` removed, the heal is not
/// heard: `` left: Some("out") right: Some("in") ``; with `drop_withdrawn` not called, the
/// withdrawn word is given to readers: `` left: Some("unknown") right: None ``.
#[test]
fn an_auth_word_heals_and_withdraws_a_macs_sign_in_word() {
    const MAC: &str = "000000000000000000000000000000aa";
    let ann = account("ann@example.test");
    let auth = |t, why: &str| Event {
        why: Word::new(why),
        ..ev(t, Kind::Auth, ann)
    };
    let word = |events: &[Event], uses: &[(Pk, Value)]| {
        Seen::of_published(MAC, events, uses, 1_000)
            .signins
            .get(ann.as_str())
            .and_then(|by_mac| by_mac.get(MAC))
            .map(|signed| signed.state)
    };
    let out = ev(100, Kind::SignedOut, ann);
    assert_eq!(word(&[out, auth(200, "ok")], &[]), Some("in"), "healed");
    assert_eq!(word(&[out, auth(200, "epoch")], &[]), Some("out"));
    // The batch a peer reads is in event order, and a week's tables come in any order.
    let healed = ev(150, Kind::SignedIn, ann);
    assert_eq!(word(&[auth(200, "unseen"), out, healed], &[]), None);
    let stale = aterm_json::json!({"signin": {"state": "in", "t": 150, "cli": "claude"}});
    assert_eq!(word(&[out, auth(200, "unseen")], &[(ann, stale)]), None);
    assert_eq!(
        word(&[auth(200, "unseen"), ev(300, Kind::Renewed, ann)], &[]),
        Some("in"),
        "a later sign-in is a word again"
    );
}

fn plan_seen(t: u64, account: Pk, plan: &str) -> Event {
    Event {
        plan: Word::new(plan),
        ..ev(t, Kind::PlanSeen, account)
    }
}

/// THE PLAN A MAC'S CLI REPORTED IS READ FROM THE WINDOW, WITH THE MAC AND THE TIME: the newest
/// `plan-seen` stands, whichever Mac logged it and in whatever order the records were folded;
/// one dated past the skew bound ahead of the read is not believed, and one inside it holds
/// from the read; an account nobody reported a plan for has none.
/// NEGATIVE CONTROLS: with `fold_event` not reading `plan-seen` (as before), `ann has a plan`
/// fails; with `plan_seen` taking the last record folded instead of the newest, the reversed
/// order reads `left: ("pro", 9500, "0303…") right: ("max", 9900, "0707…")`.
#[test]
fn the_newest_plan_a_mac_reported_stands_with_its_mac_and_time() {
    let (ann, bob) = (account("ann@example.test"), account("bob@example.test"));
    let (m3, m7) = ("03".repeat(16), "07".repeat(16));
    let now = 10_000;
    let events = [
        (m3.clone(), plan_seen(now - 500, ann, "pro")),
        (m7.clone(), plan_seen(now - 100, ann, "max")),
        // A clock an hour ahead says nothing of now; one five minutes ahead holds from now.
        (m3.clone(), plan_seen(now + 3_600, ann, "team")),
        (m7.clone(), plan_seen(now + 300, bob, "plus")),
    ];
    let mut reversed = events.clone();
    reversed.reverse();
    for events in [&events[..], &reversed[..]] {
        let seen = Seen::of_events(events, now);
        let plan = seen.plan(ann.as_str()).expect("ann has a plan");
        assert_eq!(
            (plan.word.as_str(), plan.t, plan.uuid.as_str()),
            ("max", now - 100, m7.as_str())
        );
        let plan = seen.plan(bob.as_str()).expect("bob has a plan");
        assert_eq!((plan.word.as_str(), plan.t), ("plus", now));
        assert_eq!(seen.plan(account("cat@example.test").as_str()), None);
    }
}

/// A `use` row's plan (`plan: {word, t}`, what a Mac's book states of the account every week,
/// past the weeks that held the event) is read as an event's is, under the row's own key: a
/// workspace's plan is its sub-account's, and stands for the account only where the account has
/// none of its own. A plan that is no word of the vocabulary is no plan.
/// NEGATIVE CONTROL: with `fold_use` reading only `quota` and `wall` (as before), `the
/// workspace's plan` fails.
#[test]
fn a_use_rows_plan_is_read_and_a_workspaces_stands_only_where_the_account_has_none() {
    let ann = account("ann@example.test");
    let mac = "03".repeat(16);
    let sa = keys::subaccount_pk(ann.as_str(), "0123456789ab").unwrap();
    let row = |word: &str, t: u64| aterm_json::json!({"plan": {"word": word, "t": t}});

    let mut seen = Seen::empty(1_000);
    seen.sub_of.insert(sa.clone(), ann.as_str().to_owned());
    seen.fold_use(&sa, &mac, &row("team", 900));
    let plan = seen.plan(ann.as_str()).expect("the workspace's plan");
    assert_eq!((plan.word.as_str(), plan.t), ("team", 900));
    // The account's own row stands over its workspace's, however old.
    seen.fold_use(ann.as_str(), &mac, &row("max", 400));
    let plan = seen.plan(ann.as_str()).expect("the row's plan");
    assert_eq!(
        (plan.word.as_str(), plan.t, plan.uuid.as_str()),
        ("max", 400, mac.as_str())
    );
    // An older row never replaces a newer one, and text outside the vocabulary is no plan.
    seen.fold_use(ann.as_str(), &mac, &row("pro", 300));
    seen.fold_use(ann.as_str(), &mac, &row("Max 20x <ann@example.test>", 950));
    seen.fold_use(
        ann.as_str(),
        &mac,
        &aterm_json::json!({"plan": {"word": "pro"}}),
    );
    assert_eq!(
        seen.plan(ann.as_str()).map(|plan| plan.word.as_str()),
        Some("max")
    );
}

/// END TO END on a scratch lab: the plan a Mac noted is in what it flushes (the `ev` batch and
/// its `use` row), and a read of the window gives it back with that Mac and the time.
#[test]
fn a_noted_plan_is_flushed_and_read_back() {
    let lab = Lab::new(&[Role::Writer]);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spool = Spool::at(scratch.path().join("alab"));
    let ann = account("ann@example.test");
    let now = W * WEEK + 3_600;
    // A sign-in is sharp, so the pass sends the batch at once; a plan alone waits for the
    // routine flush.
    spool.append(&ev(now - 600, Kind::SignedIn, ann)).unwrap();
    spool.append(&plan_seen(now - 590, ann, "max")).unwrap();
    let pass = |now| flush::pass(&lab.paths[0], &spool, &Presence::default, false, now).unwrap();
    // The first pass states this Mac's presence; the next sends what waits.
    assert!(matches!(pass(now), Flushed::Wrote { events: 0, .. }));
    assert!(matches!(pass(now + 60), Flushed::Wrote { events: 2, .. }));
    let seen = read(&lab.paths[0], None, now + 120).unwrap().unwrap();
    let plan = seen.plan(ann.as_str()).expect("the plan the Mac noted");
    assert_eq!((plan.word.as_str(), plan.t), ("max", now - 590));
    assert!(seen.me.contains(&plan.uuid), "this Mac read it");
}

/// The Mac that logs the walls of [`a_models_wall_stands_apart_from_the_accounts_and_from_every_other_models`].
const WALLS_MAC: &str = "0a0b0c0d0e0f101112131415161718ff";

/// A wall of `account`'s `window` seen at `t`, standing until `until`, of `model`'s own week
/// where it names one.
fn wall_of(t: u64, account: Pk, window: Window, model: Option<ModelBucket>, until: u64) -> Event {
    Event {
        window: Some(window),
        model,
        until: Some(until),
        ..ev(t, Kind::Wall, account)
    }
}

/// The clearing at `t` of `account`'s `window` wall of `model`'s week where it names one.
fn cleared(t: u64, account: Pk, window: Window, model: Option<ModelBucket>) -> Event {
    Event {
        window: Some(window),
        model,
        ..ev(t, Kind::WallCleared, account)
    }
}

/// A Mac still on a build before walls named their model (FL-269).
const OLDER_MAC: &str = "0a0b0c0d0e0f1011121314151617180e";

/// A refill at `t` of `account`'s `7d-model` window, of `model`'s week where it names one.
fn refill_of(t: u64, account: Pk, model: Option<ModelBucket>) -> Event {
    Event {
        model,
        ..refill(t, account, Window::WeekModel)
    }
}

/// Each standing wall `seen` holds of `account`: its window, the model it names, and whether it
/// walls every session of the account.
fn walls_of(seen: &Seen, account: Pk) -> Vec<(Window, Option<ModelBucket>, bool)> {
    seen.quota(account.as_str())
        .iter()
        .filter(|q| q.wall.is_some())
        .map(|q| (q.window, q.model, q.walls_account()))
        .collect()
}

/// A MODEL'S OWN WEEKLY WALL STANDS APART FROM THE ACCOUNT'S AND FROM EVERY OTHER MODEL'S
/// (FL-271). In `Seen`: ann's Fable week walled at t100 and her Opus week at t110, then an Opus
/// `wall-cleared` at t200, leave the Fable wall standing, as Fable's own row of the `7d-model`
/// window and no wall of the account's, and a refill of her Sonnet week, which names its model,
/// leaves it too, though it cuts the window's readings, which name none; a refill of Fable's
/// week ends Fable's wall. A `seven_day_other` wall is its own row and walls every session. An
/// older build's `7d-model` wall that names no model (read from its line, no `m`) reads as
/// before, one unnamed wall of the window: a clearing that names a model clears it, as any
/// clearing of the window did, and a clearing naming none clears it and no model's; the same
/// Mac's wall that names its model (its backfill's) retires it, in either fold order, while
/// another Mac's stays beside a named one; a refill that names no model (an older build's) ends
/// every model's wall. A `use` row's walls are keyed the same way, a bucket word this build does
/// not know read as `seven_day_other`'s and an `m` on a 5-hour wall no model's. In the fleet
/// table, ann's row is not walled by her Fable wall alone and names it beside (`model_walls`,
/// with its end and who hit it); dan's 5-hour wall walls his row with his Fable wall still
/// beside it, so neither hides the other, the longest of his own and his workspace's; bob's
/// `seven_day_other` wall walls his row and is named beside; eve, whom the journal never names,
/// has no `model_walls`; a refold is due at ann's Fable wall's end, and at it her wall is gone.
/// NEGATIVE CONTROLS, run: with `Seen`'s walls keyed by window alone (`wall_key` giving
/// `(window, None)`), the Opus clearing removes the standing Fable wall: `` left: [] right:
/// [(WeekModel, Some(Fable), false)] ``; with a refill's cut keyed by window alone again (every
/// refill of `7d-model` ending every model's wall), the Sonnet refill ends ann's Fable wall
/// (`` left: [] right: [(WeekModel, Some(Fable), false)] ``); with a named clearing sparing the
/// unnamed wall, cid's older wall stands (`` left: [(WeekModel, None, false)] right: [] ``), and
/// so it does with a clearing folded by removing the wall it finds (as before), since cid's
/// older Mac's log is folded after the clearing; with `Seen::uncleared` not taking the same
/// Mac's named wall as saying its unnamed one again, fay's stands twice (`` left: [(WeekModel,
/// None, false), (WeekModel, Some(Fable), false)] ``); with it taking any Mac's, gus's older
/// wall is gone; with an unnamed clearing clearing every model's wall of its window, cid's later
/// Fable wall goes (`` left: [] ``); with `Event::from_value` refusing a `7d-model` wall that names
/// no model, cid's line does not read; with `wall_key` keeping an `m` on any window, the use
/// row's 5-hour wall is lost (`left` lacks `(FiveHour, None, true)`); with the fleet row's
/// `walled` taking every window's wall again (no `walls_account` filter), ann's row reads walled
/// by her Fable wall: `` left: Some(true) right: Some(false) ``; with `model_walls` keeping a
/// bucket's shortest wall, dan's names his own week's end, not his workspace's; with it stated
/// for an account the journal does not know, eve's is `Some([])`; with no refold at a model's
/// wall's end, the next change is bob's (`` left: Some(1800345600000) right:
/// Some(1800259200000) ``); with `model_walls` not dropping an ended wall, ann's Fable wall is
/// still named at its end.
#[test]
fn a_models_wall_stands_apart_from_the_accounts_and_from_every_other_models() {
    let now = 1_800_000_000;
    let week = now + 3 * 86_400;
    let (ann, bob, cid, dan, fay, gus) = (
        account("ann@example.test"),
        account("bob@example.test"),
        account("cid@example.test"),
        account("dan@example.test"),
        account("fay@example.test"),
        account("gus@example.test"),
    );
    let (fable, opus) = (Some(ModelBucket::Fable), Some(ModelBucket::Opus));
    let by = |mac: &str, events: Vec<Event>| -> Vec<(String, Event)> {
        events
            .into_iter()
            .map(|event| (mac.to_owned(), event))
            .collect()
    };
    let mut events = by(
        WALLS_MAC,
        vec![
            wall_of(now - 900, ann, Window::WeekModel, fable, week),
            wall_of(now - 890, ann, Window::WeekModel, opus, week),
            cleared(now - 800, ann, Window::WeekModel, opus),
            Event {
                window: Some(Window::WeekModel),
                pct_x10: Some(900),
                reset: Some(week),
                res: Some(Res::Exact),
                src: Source::SessionUsage,
                ..ev(now - 300, Kind::Quota, ann)
            },
            wall_of(
                now - 700,
                bob,
                Window::WeekModel,
                Some(ModelBucket::Other),
                week + 86_400,
            ),
            cleared(now - 800, cid, Window::WeekModel, fable),
            wall_of(now - 600, dan, Window::FiveHour, None, now + 3_600),
            wall_of(now - 500, dan, Window::WeekModel, fable, week),
            // His workspace's Fable week stands longer than his own.
            Event {
                sub: Word::new("0123456789ab"),
                ..wall_of(now - 400, dan, Window::WeekModel, fable, week + 2 * 86_400)
            },
            // fay's Mac noted her wall naming no model, then, upgraded, its backfill noted it
            // again, at its time, with its model.
            wall_of(now - 900, fay, Window::WeekModel, None, week),
            wall_of(now - 900, fay, Window::WeekModel, fable, week),
            wall_of(now - 800, gus, Window::WeekModel, fable, week),
        ],
    );
    // A Mac still on an older build: its lines name no model, and read all the same.
    events.extend(by(
        OLDER_MAC,
        vec![
            Event::from_value(&wall_of(now - 900, cid, Window::WeekModel, None, week).to_value())
                .expect("a line with no `m` reads"),
            wall_of(now - 900, gus, Window::WeekModel, None, week),
        ],
    ));
    let seen = Seen::of_events(&events, now);

    // The Opus clearing cleared Opus's wall alone: Fable's still stands, Fable's own.
    assert_eq!(
        walls_of(&seen, ann),
        [(Window::WeekModel, fable, false)],
        "an Opus clearing leaves the Fable wall"
    );
    let row = seen
        .quota(ann.as_str())
        .into_iter()
        .find(|q| q.model == fable)
        .expect("Fable's own row");
    assert_eq!(row.usage, None, "a reading names no model");
    assert_eq!(row.wall.and_then(|wall| wall.until), Some(week));
    // A week no bucket names is its own, and walls every session.
    assert_eq!(
        walls_of(&seen, bob),
        [(Window::WeekModel, Some(ModelBucket::Other), true)]
    );
    // An older build's unnamed wall reads as before: a clearing of the window that names a
    // model clears it; the same Mac's named wall says it again and retires it, in either fold
    // order; another Mac's stays beside a named one.
    assert_eq!(walls_of(&seen, cid), [], "a named clearing clears it");
    assert_eq!(
        walls_of(&seen, fay),
        [(Window::WeekModel, fable, false)],
        "its own Mac's backfill retires it"
    );
    let mut stale_use = seen.clone();
    stale_use.fold_use(
        fay.as_str(),
        WALLS_MAC,
        &aterm_json::json!({"wall": [{"w": "7d-model", "until": week, "t": now - 900}]}),
    );
    assert_eq!(
        walls_of(&stale_use, fay),
        [(Window::WeekModel, fable, false)],
        "a use row its Mac wrote before it named its models retires too"
    );
    assert_eq!(
        walls_of(&seen, gus),
        [
            (Window::WeekModel, None, false),
            (Window::WeekModel, fable, false)
        ],
        "another Mac's unnamed wall stays"
    );
    assert_eq!(
        walls_of(&seen, dan),
        [
            (Window::FiveHour, None, true),
            (Window::WeekModel, fable, false),
            (Window::WeekModel, fable, false)
        ]
    );

    // A refill that names its model ends that model's wall alone and cuts the window's
    // readings; one that names none (an older build's) ends every model's wall; a clearing that
    // names none clears no model's wall.
    let mut later = events.clone();
    later.extend(by(
        WALLS_MAC,
        vec![
            wall_of(now - 150, cid, Window::WeekModel, fable, week),
            refill_of(now - 50, ann, Some(ModelBucket::Sonnet)),
            cleared(now - 100, fay, Window::WeekModel, fable),
            refill_of(now - 50, gus, fable),
        ],
    ));
    later.extend(by(
        OLDER_MAC,
        vec![
            cleared(now - 100, cid, Window::WeekModel, None),
            refill(now - 50, bob, Window::WeekModel),
        ],
    ));
    let later = Seen::of_events(&later, now);
    assert_eq!(
        walls_of(&later, ann),
        [(Window::WeekModel, fable, false)],
        "a Sonnet refill leaves the Fable wall"
    );
    assert!(
        later
            .quota(ann.as_str())
            .iter()
            .all(|q| q.window != Window::WeekModel || q.usage.is_none()),
        "a named refill cuts the window's readings, which name no model"
    );
    assert!(
        seen.quota(ann.as_str())
            .iter()
            .any(|q| q.window == Window::WeekModel && q.usage.is_some()),
        "CONTROL: the reading stood before the refill"
    );
    assert_eq!(walls_of(&later, cid), [(Window::WeekModel, fable, false)]);
    assert_eq!(walls_of(&later, fay), [], "its clearing leaves nothing");
    assert_eq!(
        walls_of(&later, gus),
        [],
        "a Fable refill ends Fable's wall"
    );
    assert_eq!(
        walls_of(&later, bob),
        [],
        "an unnamed refill ends every model's"
    );
    let fable_refill = Seen::of_events(
        &by(WALLS_MAC, vec![refill_of(now - 50, ann, fable)])
            .into_iter()
            .chain(events.iter().cloned())
            .collect::<Vec<_>>(),
        now,
    );
    assert_eq!(walls_of(&fable_refill, ann), [], "a Fable refill ends it");

    // A `use` row's walls stand by (window, model) too.
    let mut used = Seen::empty(now);
    let wall = |model: Option<&str>| {
        let mut entry = aterm_json::json!({"w": "7d-model", "until": week, "t": now - 60});
        if let (Some(model), aterm_json::Value::Object(entry)) = (model, &mut entry) {
            entry.insert("m".into(), aterm_json::Value::from(model));
        }
        entry
    };
    used.fold_use(
        ann.as_str(),
        WALLS_MAC,
        &aterm_json::json!({"wall": [
            wall(Some("seven_day_overage_included")),
            wall(Some("seven_day_opus")),
            wall(Some("seven_day_haiku")),
            // Only a `7d-model` wall is a model's: an `m` on any other is no model's.
            {"w": "5h", "m": "seven_day_opus", "until": now + 3_600, "t": now - 60},
        ]}),
    );
    // An older build's row names no model.
    used.fold_use(
        ann.as_str(),
        OLDER_MAC,
        &aterm_json::json!({"wall": [wall(None)]}),
    );
    assert_eq!(
        walls_of(&used, ann),
        [
            (Window::FiveHour, None, true),
            (Window::WeekModel, None, false),
            (Window::WeekModel, fable, false),
            (Window::WeekModel, opus, false),
            (Window::WeekModel, Some(ModelBucket::Other), true),
        ]
    );

    #[cfg(unix)]
    fleet_rows_wall_by_the_account_and_name_each_model_beside(seen, [ann, bob, dan], now, week);
}

/// The fleet half of [`a_models_wall_stands_apart_from_the_accounts_and_from_every_other_models`]
/// (the fleet tables are Unix's): `seen` folded into `[ann, bob, dan]`'s rows at `now`, each
/// model's wall standing until `week`.
#[cfg(unix)]
fn fleet_rows_wall_by_the_account_and_name_each_model_beside(
    seen: Seen,
    [ann, bob, dan]: [Pk; 3],
    now: u64,
    week: u64,
) {
    use crate::fleet::fold::{Inputs, Source, fold};
    use crate::fleet::rows::{AccountRow, ModelWall, Stamp};
    use crate::relation::Row;
    let fable = Some(ModelBucket::Fable);
    // The journal says nothing of eve.
    let eve = account("eve@example.test");
    // The fleet table: the account's own wall alone walls its row; each model's stands beside.
    let mut lab = crate::alab::view::LabView::default();
    for (pk, slug) in [(ann, "ann"), (bob, "bob"), (dan, "dan"), (eve, "eve")] {
        let pk = pk.as_str().to_owned();
        lab.accounts.insert(
            pk.clone(),
            crate::alab::view::Account {
                pk,
                provider: Some("anthropic".to_owned()),
                realm: Some("claude".to_owned()),
                slug: slug.to_owned(),
                name: None,
                legacy_label: None,
                login: None,
                identity: None,
                status: Some("active".to_owned()),
                plan: None,
                region: None,
                flow: None,
                browser: None,
                live: true,
            },
        );
    }
    let now_ms = now * 1_000;
    let inputs = Inputs {
        lab: Source::Live {
            value: std::sync::Arc::new(lab),
            at_ms: now_ms,
        },
        journal: Source::Live {
            value: std::sync::Arc::new(seen),
            at_ms: now_ms,
        },
        ..Inputs::default()
    };
    let tables = fold(&inputs, now_ms).tables;
    let row_in = |tables: &crate::fleet::fold::FleetTables, pk: Pk| -> AccountRow {
        tables
            .accounts
            .iter()
            .find(|row| row.account == pk.as_str())
            .expect("a row per account")
            .clone()
    };
    let row = |pk: Pk| row_in(&tables, pk);
    let fable_wall = |t: u64, until: u64| ModelWall {
        bucket: fable,
        until_ms: Some(until * 1_000),
        read: Stamp {
            at_ms: t * 1_000,
            by: WALLS_MAC[..8].to_owned(),
        },
    };
    assert_eq!(
        row(eve).model_walls,
        None,
        "no model's wall is known of an account the journal says nothing of"
    );
    // Once dan's 5-hour wall has ended, the next change is the first model's wall's end, ann's
    // Fable week, before bob's account-wide one; at it, hers goes.
    assert_eq!(
        fold(&inputs, (now + 3_600) * 1_000).next_change_ms,
        Some(week * 1_000)
    );
    assert_eq!(
        row_in(&fold(&inputs, week * 1_000).tables, ann).model_walls,
        Some(vec![]),
        "an ended model's wall is no longer named"
    );
    let ann_row = row(ann);
    assert_eq!(
        ann_row.walled,
        Some(false),
        "a Fable wall alone walls no account"
    );
    assert_eq!((ann_row.wall_until_ms, &ann_row.wall_read), (None, &None));
    assert_eq!(ann_row.model_walls, Some(vec![fable_wall(now - 900, week)]));
    let dan_row = row(dan);
    assert_eq!(
        dan_row.walled,
        Some(true),
        "a 5-hour wall walls the account"
    );
    assert_eq!(dan_row.wall_until_ms, Some((now + 3_600) * 1_000));
    assert_eq!(
        dan_row.model_walls,
        Some(vec![fable_wall(now - 400, week + 2 * 86_400)]),
        "the account's wall hides no model's, and a model's wall is its longest"
    );
    let bob_row = row(bob);
    assert_eq!(
        bob_row.walled,
        Some(true),
        "a week no bucket names walls every session"
    );
    assert_eq!(
        bob_row
            .model_walls
            .as_ref()
            .map(|walls| walls.iter().map(|wall| wall.bucket).collect::<Vec<_>>()),
        Some(vec![Some(ModelBucket::Other)])
    );
    // The cell is of its column's type.
    let schema = AccountRow::SCHEMA;
    let at = schema.position("model_walls").expect("a column");
    let crate::relation::Cell::Value(cell) = ann_row.cells().swap_remove(at) else {
        panic!("the list is read");
    };
    assert!(crate::typed::cell::admissible(
        &schema.columns[at].column.ty,
        &cell
    ));
    assert_eq!(
        cell[0]["bucket"],
        aterm_json::Value::from("seven_day_overage_included")
    );
}
