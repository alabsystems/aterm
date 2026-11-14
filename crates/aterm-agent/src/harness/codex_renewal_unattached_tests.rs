// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A root of `fixture`'s intent, `id` repeated, in `phase`, onto sign-in generation `generation`.
fn root(fixture: &Receipt, id: &str, phase: Phase, generation: u64) -> Receipt {
    Receipt {
        id: id.repeat(16),
        launch_attempted: phase != Phase::Ready,
        phase,
        generation,
        ..fixture.clone()
    }
}

/// End `root` as its last budget does when no successor attached, and return it as stored.
fn end_unattached(env: &Env, root: &Receipt) -> Receipt {
    with_receipt_writer(env, |writer| {
        write_receipt(env, root)?;
        writer.cancel(root, CancellationKind::SuccessorNeverAttached, 0, 0)?;
        read_receipt(env, &root.id).ok_or("ended")
    })
    .unwrap()
    .unwrap()
}

/// A login renewal whose successor never attached hands its session back on the login it left,
/// where the same wall asks for that same successor again. The end holds the successor off: no
/// root onto it is created until the rung passes, a second such end climbs a rung, the same end
/// noted again climbs none, and a new sign-in (another generation) is not held at all.
#[test]
fn a_login_successor_that_never_attached_is_held_off_by_rungs_until_a_new_sign_in() {
    let (_tmp, mut env, fixture, _) = super::super::tests::fixture();
    env.now = 1_000_000;
    let identity = fixture.intent.identity.clone();
    let until = |env: &Env, generation: u64| {
        with_receipt_writer(env, |writer| {
            writer.unattached_login_until(&identity, &fixture.new_auth, generation)
        })
        .unwrap()
        .unwrap()
    };
    let create = |env: &Env, receipt: &Receipt| {
        with_receipt_writer(env, |writer| writer.create(receipt)).map(|_| receipt.id.clone())
    };
    let generation = fixture.generation;
    assert_eq!(until(&env, generation), None);
    assert!(create(&env, &root(&fixture, "01", Phase::Ready, generation)).is_ok());

    let first = end_unattached(&env, &root(&fixture, "02", Phase::Committing, generation));
    for _ in 0..2 {
        with_receipt_writer(&env, |writer| writer.note_unattached_login(&first))
            .unwrap()
            .unwrap();
    }
    let held = env.now + accounts::NO_RESET_WALL_S[0];
    assert_eq!(until(&env, generation), Some(held), "noted once");
    assert_eq!(
        create(&env, &root(&fixture, "03", Phase::Ready, generation)),
        Err("successor-never-attached")
    );
    assert_eq!(until(&env, generation + 1), None);
    assert!(create(&env, &root(&fixture, "04", Phase::Ready, generation + 1)).is_ok());

    env.now = held;
    assert_eq!(until(&env, generation), None, "the rung passed");
    assert!(create(&env, &root(&fixture, "05", Phase::Ready, generation)).is_ok());
    let second = end_unattached(&env, &root(&fixture, "06", Phase::Committing, generation));
    with_receipt_writer(&env, |writer| writer.note_unattached_login(&second))
        .unwrap()
        .unwrap();
    assert_eq!(
        until(&env, generation),
        Some(env.now + accounts::NO_RESET_WALL_S[1]),
        "a second end climbs a rung"
    );
}

/// Only a retired login renewal whose cancellation says its successor never attached holds its
/// successor off: another reason, a live root, a rotation or a registered successor does not.
#[test]
fn only_a_login_renewal_ended_for_want_of_a_successor_holds_it_off() {
    let (_tmp, env, fixture, _) = super::super::tests::fixture();
    let note = |receipt: &Receipt| {
        with_receipt_writer(&env, |writer| writer.note_unattached_login(receipt))
    };
    let refused = Err("unattached-login-not-terminal");
    assert_eq!(note(&fixture), refused, "a live root");
    let stopped = root(&fixture, "11", Phase::Committing, fixture.generation);
    let stopped = with_receipt_writer(&env, |writer| {
        write_receipt(&env, &stopped)?;
        writer.cancel(&stopped, CancellationKind::OwnerStop, 0, 0)?;
        read_receipt(&env, &stopped.id).ok_or("stopped")
    })
    .unwrap()
    .unwrap();
    assert_eq!(note(&stopped), refused, "another reason");
    let mut registered = end_unattached(
        &env,
        &root(&fixture, "12", Phase::Committing, fixture.generation),
    );
    registered.successor_pid = Some(77);
    assert_eq!(note(&registered), refused, "a registered successor");
    let mut rotation = root(&fixture, "13", Phase::Committing, fixture.generation);
    rotation.intent.purpose = Purpose::WeeklyQuota;
    rotation.destination = Some(Identity::from_native("target", "user"));
    let rotation = end_unattached(&env, &rotation);
    assert_eq!(note(&rotation), refused, "a rotation is struck instead");
    assert_eq!(
        with_receipt_writer(&env, |writer| writer.unattached_login_until(
            &fixture.intent.identity,
            &fixture.new_auth,
            fixture.generation
        )),
        Ok(Some(None))
    );
}

/// The hold-off counts its ends as a wall's sightings are counted: an end while the hold still
/// stands is the same failure, a root started before it, and climbs no rung; an end after the
/// hold lapsed climbs one; one more than an episode after it lapsed starts over at the first; and
/// a root onto the successor that attached clears it, so the next end is a first again. Only a
/// login renewal whose successor registered clears it.
#[test]
fn a_login_successors_hold_off_counts_one_failure_once_and_starts_over() {
    let (_tmp, mut env, fixture, _) = super::super::tests::fixture();
    env.now = 1_000_000;
    let identity = fixture.intent.identity.clone();
    let generation = fixture.generation;
    let until = |env: &Env| {
        with_receipt_writer(env, |writer| {
            writer.unattached_login_until(&identity, &fixture.new_auth, generation)
        })
        .unwrap()
        .unwrap()
    };
    let end = |env: &Env, id: &str| {
        let ended = end_unattached(env, &root(&fixture, id, Phase::Committing, generation));
        with_receipt_writer(env, |writer| writer.note_unattached_login(&ended))
            .unwrap()
            .unwrap();
    };
    let rung = accounts::NO_RESET_WALL_S;

    end(&env, "21");
    let held = env.now + rung[0];
    assert_eq!(until(&env), Some(held), "a first end");
    env.now += 60;
    end(&env, "22");
    assert_eq!(until(&env), Some(held), "the same failure climbs no rung");
    env.now = held + 60;
    end(&env, "23");
    let climbed = env.now + rung[1];
    assert_eq!(
        until(&env),
        Some(climbed),
        "an end after the hold lapsed climbs"
    );
    env.now = climbed + accounts::WALL_EPISODE_S + 1;
    end(&env, "24");
    assert_eq!(until(&env), Some(env.now + rung[0]), "a new episode");

    let attach = |env: &Env, receipt: &Receipt| {
        with_receipt_writer(env, |writer| {
            write_receipt(env, receipt)?;
            writer.note_attached_login(receipt)
        })
    };
    let mut attached = root(&fixture, "25", Phase::Committed, generation);
    attached.successor_pid = Some(77);
    attached.successor_birth = Some(7_700);
    let mut rotation = attached.clone();
    rotation.id = "26".repeat(16);
    rotation.intent.purpose = Purpose::WeeklyQuota;
    rotation.destination = Some(Identity::from_native("target", "user"));
    let unregistered = root(&fixture, "27", Phase::Committed, generation);
    for refused in [&rotation, &unregistered] {
        assert_eq!(attach(&env, refused), Err("attached-login-not-committed"));
    }
    assert_eq!(until(&env), Some(env.now + rung[0]), "nothing attached yet");
    assert_eq!(attach(&env, &attached), Ok(Some(())));
    assert_eq!(until(&env), None, "an attach clears the hold-off");
    assert_eq!(
        attach(&env, &attached),
        Ok(Some(())),
        "clearing it again is exact"
    );
    env.now += 60;
    end(&env, "28");
    assert_eq!(
        until(&env),
        Some(env.now + rung[0]),
        "the next end is a first"
    );
}
