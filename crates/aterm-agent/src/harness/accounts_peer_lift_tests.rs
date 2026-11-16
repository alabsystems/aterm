// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A lift another Mac published (FL-117, `docs/DESIGN-disk-custody-2026-11-12.md` §6): the store
//! lifts the standing wall it names, once, and nothing else.

use super::*;

const NOW: i64 = 1_790_000_000;
const HOUR: i64 = 3600;
const PK: &str = "ac-00000000000000a1";
const PEER: &str = "n-00000000000000bb";

/// This Mac's state: `main`, keyed by [`PK`], walled on its five-hour window at `NOW` until
/// `reset` (`None`: no reset named), active and pinned.
fn walled(reset: Option<i64>) -> State {
    let mut state = State {
        machine: "0a0b".to_owned(),
        ..State::default()
    };
    assert_eq!(
        note_wall(&mut state, "main", SESSION_LIMIT, reset, NOW, NOW).len(),
        1
    );
    state.accounts.entry("main".to_owned()).or_default().pk = Some(PK.to_owned());
    state.active = Some("main".to_owned());
    state.pinned = true;
    state
}

fn lift(at: i64, reset: Option<i64>) -> PeerLift<'static> {
    PeerLift {
        peer: PEER,
        pk: PK,
        window: SESSION_LIMIT,
        at,
        reset,
    }
}

fn stands(state: &State, now: i64) -> bool {
    state.accounts["main"].limited_until(now).is_some()
}

/// A session of this Mac meets `main`'s standing five-hour wall of `reset` again at `at`: no new
/// wall, its newest sighting.
fn seen_again(state: &mut State, reset: i64, at: i64) {
    let events = note_wall(state, "main", SESSION_LIMIT, Some(reset), at, at);
    assert!(
        events.iter().all(|event| event.kind != Kind::Wall),
        "the same wall: {events:?}"
    );
    let wall = &state.accounts["main"].walls[SESSION_LIMIT];
    assert_eq!(
        (wall.at, wall.seen),
        (NOW, Some(at)),
        "its first and newest sighting"
    );
}

/// A PEER'S LIFT LIFTS THE STANDING WALL IT NAMES AND WAKES THE WAIT: on a store, the wall this
/// Mac observed before the lift, of the same window and reset, is lifted by one wall row that
/// names the peer, holds [`WALL_LIFTED`] and wakes ([`Event::wakes`]), and the transaction
/// touches the wake marker the sessions waiting on the account read their news by
/// (`Session::account_edge`). `active` and the pin stay; the mark names the peer, so this Mac's
/// lifts of its own ([`Account::newest_lifts`], what its quota row tells) are still none, while a
/// lift of its own is told, at its read's time; the same lift again lifts nothing.
/// NEGATIVE CONTROL: the same lift on a store whose account has no wall raises none, writes no
/// row and touches no marker.
#[test]
fn a_peer_lift_lifts_the_standing_wall_it_names_and_wakes_the_wait() {
    let reset = NOW + 2 * HOUR;
    let root = aterm_tempfile::TempDir::new().expect("a scratch dir");
    let store = Store::at(root.path());
    store.save(&walled(Some(reset))).expect("the store saves");
    let later = NOW + 60;
    assert!(stands(&store.load(later).expect("it reads").0, later));
    assert!(!store.wake_path().exists(), "no news yet");

    let events = store
        .transact(later, &mut None, |state| {
            Ok(note_peer_lift(state, &lift(NOW + 30, Some(reset)), later))
        })
        .expect("the transaction");
    assert_eq!(events.len(), 1, "{events:?}");
    let row = &events[0];
    assert_eq!(
        (row.kind, row.t, row.account.as_str()),
        (Kind::Wall, later, "main")
    );
    assert!(
        row.why.contains(WALL_LIFTED) && row.why.contains(PEER),
        "{}",
        row.why
    );
    assert!(row.wakes(), "a waiting session decides again");
    assert!(
        store.wake_path().exists(),
        "the waits' news: the wake marker"
    );
    assert_eq!(store.tail(8).expect("the ledger"), events);

    let (state, _) = store.load(later).expect("it reads");
    assert!(!stands(&state, later), "the wall is lifted");
    let wall = &state.accounts["main"].walls[SESSION_LIMIT];
    assert!(wall.cleared);
    assert_eq!((wall.by.as_str(), wall.until), (PEER, Some(reset)));
    assert_eq!(
        (state.active.as_deref(), state.pinned),
        (Some("main"), true),
        "active and the pin stay"
    );
    assert_eq!(
        state.accounts["main"].newest_lifts(&state.machine),
        [],
        "another Mac's lift is not this Mac's to tell"
    );
    let again = store
        .transact(later + 1, &mut None, |state| {
            Ok(note_peer_lift(
                state,
                &lift(NOW + 30, Some(reset)),
                later + 1,
            ))
        })
        .expect("the transaction");
    assert!(again.is_empty(), "one lift lifts one wall once: {again:?}");

    // This Mac's own lift is what its quota row tells, at the time of the read that proved it.
    let mut own = walled(Some(reset));
    let sample = Sample {
        session_pct: Some(20),
        session_reset: Some(reset),
        at: NOW + 10,
        ..Sample::default()
    };
    assert_eq!(note_room(&mut own, "main", &sample, NOW + 20).len(), 1);
    assert_eq!(
        own.accounts["main"].newest_lifts(&own.machine),
        [Lift {
            window: SESSION_LIMIT.to_owned(),
            at: NOW + 10,
            reset: Some(reset),
        }]
    );

    // NEGATIVE CONTROL: no wall stands, so the lift raises none and wakes nothing.
    let bare = aterm_tempfile::TempDir::new().expect("a scratch dir");
    let store = Store::at(bare.path());
    let mut state = State {
        machine: "0a0b".to_owned(),
        ..State::default()
    };
    state.accounts.entry("main".to_owned()).or_default().pk = Some(PK.to_owned());
    store.save(&state).expect("the store saves");
    let none = store
        .transact(later, &mut None, |state| {
            Ok(note_peer_lift(state, &lift(NOW + 30, Some(reset)), later))
        })
        .expect("the transaction");
    assert!(none.is_empty(), "{none:?}");
    assert!(
        store.load(later).expect("it reads").0.accounts["main"]
            .walls
            .is_empty()
    );
    assert!(!store.wake_path().exists(), "no news");
}

/// A PEER'S LIFT OLDER THAN THE WALL, OR OF ANOTHER RESET, LIFTS NOTHING: a lift at or before the
/// wall's observation (the wall may be a new one), one whose proof came before this Mac saw the
/// standing wall again (its newest sighting, not its first, is what the proof must follow), one
/// naming another reset or none where the wall named one (by the reset, one Mac's clock skew
/// cannot lift another's newer wall), one of another window or another account's key, one for a
/// wall that no longer stands, and one for an account that holds no key. None of them moves
/// `active` or the pin.
/// NEGATIVE CONTROL: the same wall is lifted by a lift after it and of its reset, the wall seen
/// again by a lift after that sighting, and a wall that named no reset by a lift after its
/// observation, whatever reset that names.
#[test]
fn a_peer_lift_older_than_the_wall_or_of_another_reset_lifts_nothing() {
    let reset = NOW + 2 * HOUR;
    let now = NOW + 60;
    let refused: [(&str, PeerLift<'static>); 8] = [
        ("at the observation", lift(NOW, Some(reset))),
        ("before the observation", lift(NOW - 1, Some(reset))),
        (
            "seen standing again after the proof",
            lift(NOW + 30, Some(reset)),
        ),
        ("another reset", lift(NOW + 30, Some(reset + HOUR))),
        ("no reset for a wall that named one", lift(NOW + 30, None)),
        (
            "another window",
            PeerLift {
                window: WEEK_LIMIT,
                ..lift(NOW + 30, Some(reset))
            },
        ),
        (
            "another account",
            PeerLift {
                pk: "ac-00000000000000b2",
                ..lift(NOW + 30, Some(reset))
            },
        ),
        ("a lapsed wall", lift(NOW + 30, Some(reset))),
    ];
    for (why, peer) in refused {
        let mut state = walled(Some(reset));
        if why == "seen standing again after the proof" {
            seen_again(&mut state, reset, NOW + 40);
        }
        let at = if why == "a lapsed wall" {
            reset + RESET_GRACE_S
        } else {
            now
        };
        let before = state.clone();
        assert!(note_peer_lift(&mut state, &peer, at).is_empty(), "{why}");
        assert_eq!(state, before, "{why}: nothing changed");
    }
    let mut unkeyed = walled(Some(reset));
    unkeyed.accounts.entry("main".to_owned()).or_default().pk = None;
    let before = unkeyed.clone();
    assert!(note_peer_lift(&mut unkeyed, &lift(NOW + 30, Some(reset)), now).is_empty());
    assert_eq!(unkeyed, before, "an account with no key");

    // NEGATIVE CONTROL.
    let mut state = walled(Some(reset));
    assert_eq!(
        note_peer_lift(&mut state, &lift(NOW + 30, Some(reset)), now).len(),
        1
    );
    let mut again = walled(Some(reset));
    seen_again(&mut again, reset, NOW + 40);
    assert_eq!(
        note_peer_lift(&mut again, &lift(NOW + 50, Some(reset)), now).len(),
        1,
        "a proof after the newest sighting lifts it"
    );
    assert!(!stands(&state, now));
    assert_eq!(
        (state.active.as_deref(), state.pinned),
        (Some("main"), true)
    );
    let mut unnamed = walled(None);
    assert!(stands(&unnamed, now));
    assert!(note_peer_lift(&mut unnamed, &lift(NOW, Some(reset)), now).is_empty());
    assert_eq!(
        note_peer_lift(&mut unnamed, &lift(NOW + 1, Some(reset)), now).len(),
        1,
        "a wall that named no reset goes by its observation"
    );
    assert!(!stands(&unnamed, now));
}

/// EACH LIFT THIS MAC TELLS NAMES ITS WINDOW AND WHEN ITS PROOF WAS SERVED (FL-117): the time its
/// quota row carries ([`Account::newest_lifts`]) is the proof's, not the lift's record: a read's
/// time ([`note_room`]), a refill's ([`note_reading`]), a reset step's claim ([`reset_outcome`]),
/// so a peer holds the wall to this Mac's own rule. One read that lifts both windows tells both,
/// and a peer that holds the five-hour wall alone is lifted by it. A wall seen standing again keeps
/// that sighting through the store's state file.
/// NEGATIVE CONTROL: the weekly lift lifts nothing on that peer, which holds no weekly wall.
#[test]
fn each_lift_this_mac_tells_names_its_window_and_when_its_proof_was_served() {
    let reset = NOW + 2 * HOUR;
    let week = NOW + 3 * 24 * HOUR;

    // One read, at NOW + 10 and recorded at NOW + 20, lifts both windows.
    let mut own = walled(Some(reset));
    let weekly = note_wall(&mut own, "main", WEEK_LIMIT, Some(week), NOW, NOW);
    assert!(
        weekly.iter().any(|event| event.kind == Kind::Wall),
        "{weekly:?}"
    );
    let sample = Sample {
        session_pct: Some(20),
        session_reset: Some(reset),
        week_pct: Some(30),
        week_reset: Some(week),
        at: NOW + 10,
        ..Sample::default()
    };
    assert_eq!(note_room(&mut own, "main", &sample, NOW + 20).len(), 2);
    let told = own.accounts["main"].newest_lifts(&own.machine);
    let proved = |window: &str, at: i64, reset: i64| Lift {
        window: window.to_owned(),
        at,
        reset: Some(reset),
    };
    assert_eq!(
        told,
        [
            proved(SESSION_LIMIT, NOW + 10, reset),
            proved(WEEK_LIMIT, NOW + 10, week)
        ],
        "both windows, at the read's time"
    );
    let mut peer = walled(Some(reset));
    let lifted: Vec<usize> = told
        .iter()
        .map(|lift| {
            note_peer_lift(
                &mut peer,
                &PeerLift {
                    peer: PEER,
                    pk: PK,
                    window: &lift.window,
                    at: lift.at,
                    reset: lift.reset,
                },
                NOW + 60,
            )
            .len()
        })
        .collect();
    assert_eq!(
        lifted,
        [1, 0],
        "the five-hour wall alone; NEGATIVE CONTROL: no weekly wall"
    );
    assert!(!stands(&peer, NOW + 60));

    // A refill read at NOW + 30 and recorded at NOW + 60, of the allowance a wall seen at NOW
    // was observed of.
    let mut refilled = State {
        machine: "0a0b".to_owned(),
        ..State::default()
    };
    let tap = |pct_x10: u32, refilled: Option<u32>, at: i64| TapRead {
        five_hour: Some(WindowRead {
            pct_x10,
            reset,
            refilled,
        }),
        at,
        seen_before: true,
        ..TapRead::default()
    };
    note_reading(&mut refilled, "main", &tap(1000, None, NOW - 60), NOW - 60);
    assert_eq!(
        note_wall(&mut refilled, "main", SESSION_LIMIT, Some(reset), NOW, NOW).len(),
        1
    );
    let events = note_reading(
        &mut refilled,
        "main",
        &tap(50, Some(1000), NOW + 30),
        NOW + 60,
    );
    assert!(
        events
            .iter()
            .any(|e| e.kind == Kind::Wall && e.why.contains(WALL_LIFTED)),
        "{events:?}"
    );
    assert_eq!(
        refilled.accounts["main"].newest_lifts(&refilled.machine),
        [proved(SESSION_LIMIT, NOW + 30, reset)],
        "the refill's time"
    );

    // A reset step claimed at NOW + 10 and done at NOW + 40.
    let mut reset_step = walled(Some(reset));
    assert_eq!(
        claim_reset(
            &mut reset_step,
            "main",
            (SESSION_LIMIT, reset),
            "s-1",
            42,
            "start",
            NOW + 10
        ),
        ResetClaimed::Taken
    );
    assert!(
        !reset_outcome(
            &mut reset_step,
            "main",
            reset,
            "s-1",
            &ResetDone::Lifted,
            NOW + 40
        )
        .is_empty()
    );
    assert_eq!(
        reset_step.accounts["main"].newest_lifts(&reset_step.machine),
        [proved(SESSION_LIMIT, NOW + 10, reset)],
        "the claim's time"
    );

    // The newest sighting is kept through the state file.
    let root = aterm_tempfile::TempDir::new().expect("a scratch dir");
    let store = Store::at(root.path());
    store.save(&walled(Some(reset))).expect("the store saves");
    store
        .transact(NOW + 40, &mut None, |state| {
            seen_again(state, reset, NOW + 40);
            Ok(Vec::new())
        })
        .expect("the transaction");
    let (state, _) = store.load(NOW + 41).expect("it reads");
    assert_eq!(
        state.accounts["main"].walls[SESSION_LIMIT].seen,
        Some(NOW + 40)
    );
}
