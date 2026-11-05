// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A launch's account claim (docs/DESIGN-fluid-swarm-2026-10-28.md F19): the row a launch starts
//! on is claimed before its session starts, and a launch on another Mac that has heard the claim
//! starts elsewhere. A launch whose claim this Mac could not record starts only on a home account,
//! for each provider participating in the shared machine claim set.

use std::sync::Arc;

use super::*;
use crate::harness::claims::{self, LeaseKey, tests::Replica};

impl World {
    /// `cli`'s roster of two with an email on each row, on a Mac whose accounts sheet is
    /// `ai-pool.toml`'s, where claims apply.
    fn claimed_two(&self, cli: Cli) {
        let work = self.work_dir(cli);
        let text = format!(
            "[[account]]\nlabel = \"main\"\nclass = \"shared\"\nemail = \"main@example.test\"\n\n\
             [[account]]\nlabel = \"work\"\nclass = \"shared\"\nemail = \"work@example.test\"\n\
             dir = {}\n",
            toml(&work)
        );
        std::fs::write(self.cfg.join(cli.accounts().roster_file), text).expect("roster");
        let secrets = self.home.join(".secrets");
        std::fs::create_dir_all(&secrets).expect("secrets dir");
        std::fs::write(
            secrets.join(crate::harness::ai_pool::AI_POOL_FILE),
            "version = 1\n",
        )
        .expect("pool");
    }
}

#[test]
fn launch_claims_before_the_first_tap() {
    let (m1, m2) = (World::new("claims-m1"), World::new("claims-m2"));
    for world in [&m1, &m2] {
        world.claimed_two(Cli::Claude);
        world.on_main(Cli::Claude, &[]);
    }
    let (t1, t2) = (
        Arc::new(Replica::new("aa01")),
        Arc::new(Replica::new("bb02")),
    );

    // m1 launches on main, and its row claims main before any usage reading.
    let pick = claims::with_table(t1.clone(), || {
        launch_env(&m1.seat(), Cli::Claude, &no_usage)
    });
    if cfg!(not(unix)) {
        // Off Unix no owner-only claim activity can be kept (`claims_activity::Held::take`), so
        // the claim is unrecorded and a non-home lease is refused with no claim row written.
        assert_eq!(pick, None);
        assert!(m1.load(Cli::Claude).unrecorded);
        assert_eq!(t1.row("aa01"), None);
        return;
    }
    assert_eq!(pick.expect("a pick").label, "main");
    let main = LeaseKey::new("claude", "main@example.test").expect("a key");
    let row = t1.row("aa01").expect("m1's launch wrote its row");
    assert_eq!(
        row.claims.iter().map(|c| &c.key).collect::<Vec<_>>(),
        [&main]
    );

    // m1's row reaches m2: m2's launch reads it first and starts on work.
    t2.merge(&t1);
    let pick = claims::with_table(t2.clone(), || {
        launch_env(&m2.seat(), Cli::Claude, &no_usage)
    });
    assert_eq!(pick.expect("a pick").label, "work");
    assert!(matches!(
        m2.load(Cli::Claude).lease_verdict("main", NOW),
        Err(accounts::WhyNot::LeasedElsewhere { .. })
    ));
    let row = t2.row("bb02").expect("m2's launch wrote its row");
    let work = LeaseKey::new("claude", "work@example.test").expect("a key");
    assert_eq!(
        row.claims.iter().map(|c| &c.key).collect::<Vec<_>>(),
        [&work]
    );
}

#[test]
fn a_launch_never_starts_on_a_lease_its_claim_could_not_record() {
    let m1 = World::new("claims-refused");
    m1.claimed_two(Cli::Claude);
    m1.on_main(Cli::Claude, &[]);
    // The read says m1 may write; each write is refused at its commit, as a full table's is.
    let full = Arc::new(Replica::refusing(
        "aa01",
        "table operation capacity exhausted",
    ));
    let pick = claims::with_table(full.clone(), || {
        super::super::launch_env(&m1.seat(), Cli::Claude, &no_usage)
    });
    assert!(pick.is_err(), "{pick:?}");
    assert!(m1.load(Cli::Claude).unrecorded);

    // Its home account is its own to use, recorded or not.
    let mut note = None;
    m1.seat()
        .store(Cli::Claude)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |state| {
            state.home_accounts.insert("work".to_owned());
            Ok(Vec::new())
        })
        .expect("a home account");
    let pick = claims::with_table(full.clone(), || {
        launch_env(&m1.seat(), Cli::Claude, &no_usage)
    });
    assert_eq!(pick.expect("a pick").label, "work");
    assert_eq!(full.row("aa01"), None);
}

#[test]
fn a_codex_launch_claims_its_account() {
    let m1 = World::new("claims-codex");
    m1.claimed_two(Cli::Codex);
    m1.on_main(Cli::Codex, &[]);
    let t1 = Arc::new(Replica::new("aa01"));
    let pick = claims::with_table(t1.clone(), || launch_env(&m1.seat(), Cli::Codex, &no_usage));
    if cfg!(not(unix)) {
        assert!(pick.is_none());
        assert!(t1.row("aa01").is_none());
        return;
    }
    assert_eq!(pick.expect("a pick").label, "main");
    assert!(
        t1.row("aa01")
            .unwrap()
            .claim(&LeaseKey::new("codex", "main@example.test").unwrap())
            .is_some()
    );
    assert!(!m1.load(Cli::Codex).unrecorded);
}

#[cfg(unix)]
#[test]
fn native_quota_commit_claims_the_destination_and_refuses_a_senior_peer() {
    use claims::Table;
    for peer_holds in [false, true] {
        let world = World::new("native-quota-machine-claim");
        world.two(Cli::Codex);
        world.claimed_two(Cli::Codex);
        world.on_main(Cli::Codex, &[]);
        let mut request = session(Cli::Codex, Supervisor::Own, Mode::Early, &world.exe);
        request.point = Point::Wall {
            window: "seven_day".into(),
            at: Some(NOW),
        };
        request.sample.as_mut().unwrap().week_pct = Some(100);
        let plan = super::super::native_quota_plan(&world.seat(), &request, |note| note.record());
        assert!(plan.selected_rows().is_some());
        let table = Arc::new(Replica::new("aa01"));
        let destination = LeaseKey::new("codex", "work@example.test").unwrap();
        if peer_holds {
            let peer = Replica::new("bb02");
            peer.write(
                &world.home,
                &claims::Row {
                    machine: "bb02".into(),
                    label: "peer".into(),
                    counter: 1,
                    until: NOW + claims::HORIZON_S,
                    claims: vec![claims::Claim {
                        key: destination.clone(),
                        kind: claims::Kind::Lease,
                        since: NOW - 1,
                    }],
                },
            )
            .unwrap();
            table.merge(&peer);
        }
        let result = claims::with_table(table.clone(), || {
            plan.commit_native(NOW, &"ab".repeat(16), &"cd".repeat(32))
        });
        assert_eq!(result.line.is_some(), !peer_holds);
        let state = world.load(Cli::Codex);
        if peer_holds {
            assert_eq!(state.active.as_deref(), Some("main"));
            assert!(state.rotations.is_empty());
            assert!(table.row("aa01").is_none());
        } else {
            assert_eq!(state.active.as_deref(), Some("work"));
            assert_eq!(state.rotations, [NOW]);
            assert!(table.row("aa01").unwrap().claim(&destination).is_some());
        }
    }
}

/// Rotation off moves nothing, and its launch on the owner's pin still never starts on another
/// Mac's account: a pin whose row the claims read held elsewhere is refused, and so is one whose
/// claim this Mac could not record on a row that is not its home. A pin that is free is claimed
/// before its session starts.
///
/// NEGATIVE CONTROL: in `launch_env_using`'s `Mode::Off` branch, drop the pin's `lease_verdict`
/// check and keep claiming after `finish_launch` with the answer thrown away (as before), and m2
/// starts on `work` beside m1, and the refusing table's launch starts on `work` unrecorded.
#[test]
fn rotation_off_never_starts_on_a_pin_another_mac_holds() {
    let (m1, m2) = (World::new("claims-off-m1"), World::new("claims-off-m2"));
    for world in [&m1, &m2] {
        world.claimed_two(Cli::Claude);
        world.on_main(Cli::Claude, &[]);
        pin(world, Cli::Claude, "work").expect("pin work");
        aterm_uds::atomic_file::write(
            &(world.cfg.join("aterm.toml")),
            ("[harness]\naccounts = \"off\"\n").as_ref(),
            0o600,
        )
        .expect("aterm.toml");
        assert_eq!(world.seat().mode_for(Cli::Claude), Mode::Off);
    }
    let (t1, t2) = (
        Arc::new(Replica::new("aa01")),
        Arc::new(Replica::new("bb02")),
    );
    let work = LeaseKey::new("claude", "work@example.test").expect("a key");

    // m1's pin is free: its launch claims work before the session starts.
    let pick = claims::with_table(t1.clone(), || {
        super::super::launch_env(&m1.seat(), Cli::Claude, &no_usage)
    });
    if cfg!(not(unix)) {
        // Off Unix no owner-only claim activity can be kept (`claims_activity::Held::take`), so
        // a non-home pin is refused until marked home, with no claim row written.
        let why = pick.expect_err("off Unix a non-home pin's claim is unrecorded");
        assert!(why.contains("cannot record a claim"), "{why}");
        assert_eq!(t1.row("aa01"), None);
        let mut note = None;
        m1.seat()
            .store(Cli::Claude)
            .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |state| {
                state.home_accounts.insert("work".to_owned());
                Ok(Vec::new())
            })
            .expect("a home account");
        let home_pick = claims::with_table(t1.clone(), || {
            super::super::launch_env(&m1.seat(), Cli::Claude, &no_usage)
        });
        assert_eq!(home_pick.expect("a launch").expect("a pick").label, "work");
        assert_eq!(t1.row("aa01"), None);
        return;
    }
    assert_eq!(pick.expect("a launch").expect("a pick").label, "work");
    let row = t1.row("aa01").expect("m1's launch wrote its row");
    assert_eq!(
        row.claims.iter().map(|c| &c.key).collect::<Vec<_>>(),
        [&work]
    );

    // m1's row reaches m2: m2's pin is m1's to use now, so m2 starts nothing and claims nothing.
    t2.merge(&t1);
    let pick = claims::with_table(t2.clone(), || {
        super::super::launch_env(&m2.seat(), Cli::Claude, &no_usage)
    });
    let why = pick.expect_err("a pin another Mac holds is refused");
    assert!(why.contains("another Mac's claim holds it"), "{why}");
    assert_eq!(t2.row("bb02"), None, "m2 wrote no claim");

    // A Mac that cannot record its claim starts on its pin only where the pin is its home.
    let m3 = World::new("claims-off-m3");
    m3.claimed_two(Cli::Claude);
    m3.on_main(Cli::Claude, &[]);
    pin(&m3, Cli::Claude, "work").expect("pin work");
    aterm_uds::atomic_file::write(
        &(m3.cfg.join("aterm.toml")),
        ("[harness]\naccounts = \"off\"\n").as_ref(),
        0o600,
    )
    .expect("aterm.toml");
    let full = Arc::new(Replica::refusing(
        "cc03",
        "table operation capacity exhausted",
    ));
    let pick = claims::with_table(full.clone(), || {
        super::super::launch_env(&m3.seat(), Cli::Claude, &no_usage)
    });
    let why = pick.expect_err("an unrecorded claim on a lease is refused");
    assert!(why.contains("cannot record a claim"), "{why}");
    let mut note = None;
    m3.seat()
        .store(Cli::Claude)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |state| {
            state.home_accounts.insert("work".to_owned());
            Ok(Vec::new())
        })
        .expect("a home account");
    let pick = claims::with_table(full.clone(), || {
        super::super::launch_env(&m3.seat(), Cli::Claude, &no_usage)
    });
    assert_eq!(pick.expect("a launch").expect("a pick").label, "work");
}

#[test]
fn cold_relaunch_claims_kept_row_and_moves_off_a_row_another_mac_holds() {
    let (m1, m2) = (World::new("claims-cold-m1"), World::new("claims-cold-m2"));
    for world in [&m1, &m2] {
        world.claimed_two(Cli::Claude);
        world.on_main(Cli::Claude, &[]);
    }
    let (t1, t2) = (
        Arc::new(Replica::new("aa01")),
        Arc::new(Replica::new("bb02")),
    );
    let main = LeaseKey::new("claude", "main@example.test").expect("a key");
    let work = LeaseKey::new("claude", "work@example.test").expect("a key");

    // m1 cold-relaunches a session that ran on main while main is free: it keeps main (`None`)
    // and claims main before the session starts.
    let pick = claims::with_table(t1.clone(), || {
        cold_launch_env(&m1.seat(), Cli::Claude, &[], &no_usage, None)
    });
    if cfg!(not(unix)) {
        // Off Unix no owner-only claim activity can be kept (`claims_activity::Held::take`), so
        // no claim row is written and cold relaunch moves only onto a home account.
        assert_eq!(pick, None);
        assert!(m1.load(Cli::Claude).unrecorded);
        assert_eq!(t1.row("aa01"), None);
        let mut note = None;
        m1.seat()
            .store(Cli::Claude)
            .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |state| {
                state.home_accounts.insert("work".to_owned());
                Ok(Vec::new())
            })
            .expect("a home account");
        let moved = claims::with_table(t1.clone(), || {
            cold_launch_env(&m1.seat(), Cli::Claude, &[], &no_usage, None)
        });
        assert_eq!(moved.expect("moved onto home work").label, "work");
        assert_eq!(t1.row("aa01"), None);
        return;
    }
    assert_eq!(
        pick, None,
        "main still serves, so the cold relaunch keeps it"
    );
    let row = t1.row("aa01").expect("m1's cold relaunch claimed main");
    assert_eq!(
        row.claims.iter().map(|c| &c.key).collect::<Vec<_>>(),
        [&main]
    );

    // m1's claim on main reaches m2: m2's cold relaunch of a session that ran on main moves onto
    // work and claims work before starting.
    t2.merge(&t1);
    let pick = claims::with_table(t2.clone(), || {
        cold_launch_env(&m2.seat(), Cli::Claude, &[], &no_usage, None)
    });
    assert_eq!(pick.expect("moved off main held by m1").label, "work");
    let row = t2.row("bb02").expect("m2's cold relaunch claimed work");
    assert_eq!(
        row.claims.iter().map(|c| &c.key).collect::<Vec<_>>(),
        [&work]
    );
}
