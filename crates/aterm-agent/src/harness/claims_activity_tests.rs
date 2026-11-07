// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::tests::Replica;
use super::super::{Act, Cli, Kind, Mine, Touched, claimed, touch_on, touch_using};
use super::*;

const NOW: i64 = 1_790_000_000;

fn key() -> LeaseKey {
    LeaseKey::new("claude", "shared@example.test").unwrap()
}

struct Lab {
    root: aterm_tempfile::TempDir,
    table: Replica,
    roster: PathBuf,
}

impl Lab {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let roster = root.path().join("roster.toml");
        std::fs::write(
            &roster,
            format!(
                "[[account]]\nlabel='alpha'\nclass='shared'\nemail='shared@example.test'\n\n\
                [[account]]\nlabel='beta'\nclass='shared'\nemail='other@example.test'\ndir='{}'\n",
                root.path().join("beta").display()
            ),
        )
        .unwrap();
        Self {
            root,
            table: Replica::new("aa01"),
            roster,
        }
    }

    fn state(&self, name: &str) -> accounts::Store {
        accounts::Store::at(&self.root.path().join(name))
    }

    fn touch(&self, name: &str, act: Act<'_>, now: i64) -> Touched {
        touch_on(
            &self.table,
            self.root.path(),
            &self.root.path().join(name),
            Some(&self.roster),
            Cli::Claude,
            act,
            now,
        )
    }

    fn row(&self) -> Row {
        self.table.row("aa01").unwrap()
    }
}

#[test]
fn shared_claim_entrypoint_publishes_codex_and_refuses_a_second_machine() {
    for cli in super::super::CLAIMED_CLIS {
        let first = Lab::new();
        let second = Lab::new();
        for lab in [&first, &second] {
            let secrets = lab.root.path().join(".secrets");
            std::fs::create_dir(&secrets).unwrap();
            std::fs::write(secrets.join(crate::harness::ai_pool::AI_POOL_FILE), "").unwrap();
        }
        let first_table = std::sync::Arc::new(Replica::new("aa01"));
        let second_table = std::sync::Arc::new(Replica::new("bb02"));
        let claim = |lab: &Lab, table: std::sync::Arc<Replica>| {
            super::super::with_table(table, || {
                super::super::touch(
                    lab.root.path(),
                    &lab.root.path().join("state"),
                    Some(&lab.roster),
                    cli,
                    Act::Claim("alpha"),
                    NOW,
                )
            })
        };
        assert_eq!(claim(&first, first_table.clone()), Touched::Claimed);
        let identity = LeaseKey::for_cli(
            cli,
            Some(crate::ai::connector::Region::Global),
            "shared@example.test",
        )
        .unwrap();
        assert!(first_table.row("aa01").unwrap().claim(&identity).is_some());
        second_table.merge(&first_table);
        assert!(matches!(claim(&second, second_table.clone()),
            Touched::HeldElsewhere(holder) if holder.machine == "aa01"));
        assert!(second_table.row("bb02").is_none());
    }
}

#[test]
fn roster_and_table_reads_share_the_account_writer_lock() {
    use super::super::{Table, Tabled, touch_reading};

    struct LockedTable<'a> {
        table: &'a Replica,
        store: &'a accounts::Store,
    }
    impl Table for LockedTable<'_> {
        fn read(&self, home: &Path) -> Result<Tabled, String> {
            assert!(self.store.try_acquire().unwrap().is_none());
            self.table.read(home)
        }

        fn write(&self, home: &Path, row: &Row) -> Result<(), String> {
            assert!(self.store.try_acquire().unwrap().is_none());
            self.table.write(home, row)
        }
    }

    let lab = Lab::new();
    let store = lab.state("first");
    assert!(store.try_acquire().unwrap().is_some());
    let table = LockedTable {
        table: &lab.table,
        store: &store,
    };
    let result = touch_reading(
        &table,
        lab.root.path(),
        &store,
        Cli::Claude,
        Act::Claim("alpha"),
        NOW,
        || {
            // The old pre-transaction roster read fails this same probe.
            assert!(store.try_acquire().unwrap().is_none());
            accounts::Roster::load(&lab.roster)
        },
    );
    assert_eq!(result, Touched::Claimed);
    assert!(store.try_acquire().unwrap().is_some());
    assert!(lab.row().claim(&key()).is_some());
}

#[test]
fn unreadable_bindings_refuse_before_activity_or_claim_publication() {
    for missing_snapshot in [false, true] {
        let lab = Lab::new();
        let store = lab.state("first");
        store.ensure().unwrap();
        let invalid = if missing_snapshot {
            store.logins_witness_file()
        } else {
            store.logins_file()
        };
        std::fs::write(&invalid, "retained invalid record").unwrap();
        let result = lab.touch("first", Act::Claim("alpha"), NOW);
        assert!(matches!(result, Touched::Unrecorded(_)), "{result:?}");
        assert!(lab.table.row("aa01").is_none());
        assert!(!lab.root.path().join(".aterm/claim-activity").exists());
        assert_eq!(
            std::fs::read_to_string(invalid).unwrap(),
            "retained invalid record"
        );
    }
}

#[test]
fn malformed_roster_refuses_but_an_absent_roster_remains_empty() {
    let lab = Lab::new();
    std::fs::write(&lab.roster, "[[account").unwrap();
    let result = lab.touch("first", Act::Claim("alpha"), NOW);
    assert!(matches!(result, Touched::Unrecorded(_)), "{result:?}");
    assert!(lab.table.row("aa01").is_none());
    let result = touch_on(
        &lab.table,
        lab.root.path(),
        &lab.root.path().join("second"),
        Some(&lab.root.path().join("absent.toml")),
        Cli::Claude,
        Act::Look,
        NOW,
    );
    assert_eq!(result, Touched::Looked);
    assert!(lab.table.row("aa01").is_none());
}

#[test]
fn independent_stores_preserve_activity_and_seniority_then_release_at_expiry() {
    let lab = Lab::new();
    lab.state("first")
        .transact(NOW, &mut None, |state| {
            Ok(accounts::note_session(state, "s-first", "alpha", None, NOW))
        })
        .unwrap();
    assert_eq!(
        lab.touch("first", Act::Claim("alpha"), NOW),
        Touched::Claimed
    );
    assert_eq!(
        lab.touch("second", Act::Claim("beta"), NOW + 1),
        Touched::Claimed
    );
    assert_eq!(
        lab.row().claims.len(),
        2,
        "second store cannot retract the first's activity"
    );

    // A different roster has no label for alpha. It can still expire its own unused beta.
    std::fs::write(
        &lab.roster,
        "[[account]]\nlabel='beta'\nclass='shared'\nemail='other@example.test'\n",
    )
    .unwrap();
    assert_eq!(
        lab.touch("second", Act::Look, NOW + IDLE_S + 2),
        Touched::Looked
    );
    let row = lab.row();
    assert_eq!(row.claims.len(), 1);
    assert_eq!(row.claim(&key()).unwrap().since, NOW);
    assert_eq!(
        lab.touch("second", Act::Look, NOW + SESSION_FRESH_S),
        Touched::Looked
    );
    assert!(
        lab.row().claims.is_empty(),
        "positive evidence does not live forever"
    );
}

#[test]
fn a_preparation_hold_refuses_claims_without_poisoning_an_authorized_store() {
    let lab = Lab::new();
    lab.state("prepared")
        .transact(NOW, &mut None, |state| {
            state.prepared_accounts.insert("alpha".into());
            Ok(Vec::new())
        })
        .unwrap();
    assert_eq!(
        lab.touch("prepared", Act::Claim("alpha"), NOW),
        Touched::PreparationOnly
    );
    assert!(lab.table.row("aa01").is_none());
    assert!(
        lab.state("prepared")
            .load(NOW)
            .unwrap()
            .0
            .claimed_at
            .is_empty()
    );
    let held = Held::take(lab.root.path()).unwrap();
    assert!(!held.live(&key(), NOW));
    drop(held);
    assert_eq!(
        lab.touch("authorized", Act::Claim("alpha"), NOW + 1),
        Touched::Claimed
    );
}

#[test]
fn preparation_holds_stop_refreshing_prior_claim_evidence() {
    let lab = Lab::new();
    let store = lab.state("first");
    store
        .transact(NOW, &mut None, |state| {
            Ok(accounts::note_session(state, "s-first", "alpha", None, NOW))
        })
        .unwrap();
    assert_eq!(
        lab.touch("first", Act::Claim("alpha"), NOW),
        Touched::Claimed
    );
    store
        .transact(NOW + 1, &mut None, |state| {
            state.prepared_accounts.insert("alpha".into());
            Ok(Vec::new())
        })
        .unwrap();
    assert_eq!(lab.touch("first", Act::Look, NOW + 2), Touched::Looked);
    let held = Held::take(lab.root.path()).unwrap();
    assert_eq!(
        held.book.as_ref().unwrap().entries[&key()],
        NOW + SESSION_FRESH_S
    );
    drop(held);
    // A quiet where this Mac has a row reads `Quieted`, so its row is pushed to the peers.
    assert_eq!(
        lab.touch("first", Act::Quiet, NOW + SESSION_FRESH_S),
        Touched::Quieted
    );
    assert!(lab.row().claims.is_empty());
}

#[test]
fn a_home_accounts_preparation_refusal_survives_unreadable_activity() {
    let lab = Lab::new();
    lab.state("prepared")
        .transact(NOW, &mut None, |state| {
            state.prepared_accounts.insert("alpha".into());
            state.home_accounts.insert("alpha".into());
            Ok(Vec::new())
        })
        .unwrap();
    drop(Held::take(lab.root.path()).unwrap());
    private_file::write_private(
        &lab.root.path().join(".aterm/claim-activity"),
        FILE,
        b"bad json",
    )
    .unwrap();
    assert_eq!(
        lab.touch("prepared", Act::Claim("alpha"), NOW),
        Touched::PreparationOnly
    );
    assert!(lab.table.row("aa01").is_none());
    assert!(
        lab.state("prepared")
            .load(NOW)
            .unwrap()
            .0
            .claimed_at
            .is_empty()
    );
}

#[test]
fn a_home_accounts_preparation_refusal_survives_a_rejected_roster() {
    let lab = Lab::new();
    lab.state("prepared")
        .transact(NOW, &mut None, |state| {
            state.prepared_accounts.insert("alpha".into());
            state.home_accounts.insert("alpha".into());
            Ok(Vec::new())
        })
        .unwrap();
    std::fs::write(&lab.roster, "[[account").unwrap();
    assert_eq!(
        lab.touch("prepared", Act::Claim("alpha"), NOW),
        Touched::PreparationOnly
    );
    assert!(lab.table.row("aa01").is_none());
    assert!(
        lab.state("prepared")
            .load(NOW)
            .unwrap()
            .0
            .claimed_at
            .is_empty()
    );
}

#[test]
fn a_senior_peer_refusal_survives_failure_to_save_the_observation() {
    use super::super::{Table, Tabled};
    let lab = Lab::new();
    peer_claim(&lab, NOW + super::super::HORIZON_S);
    struct FailingSave<'a> {
        table: &'a Replica,
        state: PathBuf,
    }
    impl Table for FailingSave<'_> {
        fn read(&self, home: &Path) -> Result<Tabled, String> {
            let read = self.table.read(home)?;
            std::fs::create_dir(&self.state).unwrap();
            Ok(read)
        }
        fn write(&self, _: &Path, _: &Row) -> Result<(), String> {
            panic!("a peer refusal must not publish a claim");
        }
    }
    let table = FailingSave {
        table: &lab.table,
        state: lab.state("first").state_file(),
    };
    let result = touch_on(
        &table,
        lab.root.path(),
        &lab.root.path().join("first"),
        Some(&lab.roster),
        Cli::Claude,
        Act::Claim("alpha"),
        NOW,
    );
    assert!(matches!(result, Touched::HeldElsewhere(_)), "{result:?}");
    assert!(table.state.is_dir(), "the state save was obstructed");
    assert!(lab.table.row("aa01").is_none());
}

#[test]
fn negative_control_without_shared_evidence_drops_the_other_stores_live_claim() {
    use super::super::touch_with;
    let lab = Lab::new();
    let roster = accounts::Roster::load_for(Cli::Claude, &lab.roster).unwrap();
    let keys = super::super::keys(Cli::Claude, &roster);
    let mut first = accounts::State::default();
    let mut second = accounts::State::default();
    assert_eq!(
        touch_with(
            &lab.table,
            lab.root.path(),
            None,
            &mut first,
            Cli::Claude,
            &keys,
            Act::Claim("alpha"),
            NOW
        ),
        Touched::Claimed
    );
    assert_eq!(
        touch_with(
            &lab.table,
            lab.root.path(),
            None,
            &mut second,
            Cli::Claude,
            &keys,
            Act::Claim("beta"),
            NOW + 1
        ),
        Touched::Claimed
    );
    assert!(
        lab.row().claim(&key()).is_none(),
        "the old local-only policy loses alpha"
    );
}

#[test]
fn alias_and_restarted_store_keep_a_tapped_claim_past_the_original_session() {
    let lab = Lab::new();
    assert_eq!(
        lab.touch("first", Act::Claim("alpha"), NOW),
        Touched::Claimed
    );
    let renewed = NOW + IDLE_S - 1;
    assert_eq!(
        lab.touch("first", Act::Claim("alpha"), renewed),
        Touched::Held
    );
    std::fs::write(
        &lab.roster,
        "[[account]]\nlabel='alias'\nclass='shared'\nemail='shared@example.test'\n",
    )
    .unwrap();
    assert_eq!(
        lab.touch("second", Act::Look, NOW + IDLE_S + 1),
        Touched::Looked
    );
    assert_eq!(lab.row().claim(&key()).unwrap().since, NOW);
    assert_eq!(
        lab.touch("second", Act::Claim("alias"), NOW + IDLE_S + 2),
        Touched::Held
    );
    assert_eq!(lab.row().claim(&key()).unwrap().since, NOW);
}

#[test]
fn sleep_preserves_other_stores_live_claims_and_wake_renews_the_whole_row() {
    let lab = Lab::new();
    assert_eq!(
        lab.touch("first", Act::Claim("alpha"), NOW),
        Touched::Claimed
    );
    assert_eq!(
        lab.touch("second", Act::Claim("beta"), NOW),
        Touched::Claimed
    );
    assert_eq!(lab.touch("second", Act::Quiet, NOW + 1), Touched::Quieted);
    assert_eq!(lab.row().claims.len(), 2);
    assert_eq!(lab.row().until, NOW + 1 + SESSION_FRESH_S);
    assert_eq!(
        lab.touch("first", Act::Claim("alpha"), NOW + 2),
        Touched::Claimed
    );
    assert_eq!(lab.row().claims.len(), 2);
    assert_eq!(lab.row().claim(&key()).unwrap().since, NOW);
    assert_eq!(lab.row().until, NOW + 2 + super::super::HORIZON_S);
}

/// A claims table whose push hands its rows to `peer`, as a peer's next repair round would, unless
/// `fail` says no peer took it.
struct Pushing<'a> {
    table: &'a Replica,
    peer: &'a Replica,
    pushes: std::sync::atomic::AtomicUsize,
    fail: std::sync::atomic::AtomicBool,
}

impl<'a> Pushing<'a> {
    fn new(table: &'a Replica, peer: &'a Replica) -> Self {
        Self {
            table,
            peer,
            pushes: std::sync::atomic::AtomicUsize::new(0),
            fail: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn pushes(&self) -> usize {
        self.pushes.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// `act` as the Mac goes quiet or down, on `store` (`touch_pushing`'s work).
    fn down(&self, lab: &Lab, store: &str, act: Act<'_>, now: i64) -> super::super::Pushed {
        super::super::push_on(
            self,
            lab.root.path(),
            &lab.root.path().join(store),
            Some(&lab.roster),
            Cli::Claude,
            act,
            now,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
    }
}

impl super::super::Table for Pushing<'_> {
    fn read(&self, home: &Path) -> Result<super::super::Tabled, String> {
        self.table.read(home)
    }

    fn write(&self, home: &Path, row: &Row) -> Result<(), String> {
        self.table.write(home, row)
    }

    fn push(&self, _: &Path, _: std::time::Instant) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        self.pushes.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err("no peer took it".to_owned());
        }
        self.peer.merge(self.table);
        Ok(())
    }
}

/// Whether `peer` reads account `alpha` free.
fn free_on(peer: &Replica, now: i64) -> bool {
    let view = super::super::View {
        me: "bb02".into(),
        presence: None,
        ..super::super::View::default()
    };
    let book = super::super::Book::of(peer.row("aa01"));
    book.senior(&key(), &view, now).is_none()
}

/// A quiet or off row goes to the peers at once ([`super::super::Table::push`]): the peer's replica
/// holds it as the touch returns, the claim kept there shortened, then given up and free. An off
/// with nothing left to give up writes nothing and still pushes the row.
///
/// NEGATIVE CONTROL: make `push_on` skip the push. With `pushed = None` the quiet returns
/// `(Quieted, None)`; with the skipped push still reported as `Some(Ok(()))`, the peer holds the
/// row from before the quiet: the claim stands there to its old `until`.
#[test]
fn a_quiet_or_off_row_is_pushed_to_the_peers_at_once() {
    use super::super::HORIZON_S;

    let lab = Lab::new();
    let peer = Replica::new("bb02");
    let pushing = Pushing::new(&lab.table, &peer);
    assert_eq!(
        lab.touch("first", Act::Claim("alpha"), NOW),
        Touched::Claimed
    );
    peer.merge(&lab.table);
    assert_eq!(peer.row("aa01").map(|row| row.until), Some(NOW + HORIZON_S));

    // Asleep, the claim in use is kept, two hours at most, on the peer too.
    assert_eq!(
        pushing.down(&lab, "first", Act::Quiet, NOW + 1),
        (Touched::Quieted, Some(Ok(())))
    );
    assert_eq!(
        peer.row("aa01").map(|row| row.until),
        Some(NOW + 1 + SESSION_FRESH_S)
    );

    // Down, every claim goes, on the peer too.
    assert_eq!(
        pushing.down(&lab, "first", Act::Off, NOW + 2),
        (Touched::Quieted, Some(Ok(())))
    );
    let row = peer.row("aa01").expect("the pushed row");
    assert!(row.claims.is_empty(), "{row:?}");
    assert!(free_on(&peer, NOW + 2));

    // Nothing left to give up: nothing written, the row pushed again.
    assert_eq!(
        pushing.down(&lab, "first", Act::Off, NOW + 3),
        (Touched::Quieted, Some(Ok(())))
    );
    assert_eq!(lab.row().counter, row.counter);
    assert_eq!(pushing.pushes(), 3);
}

/// An off whose push reached no peer is not the row's last chance: the next quiet or off (the node
/// job's, after the window's) finds nothing left to give up and pushes the row all the same, so
/// the peer reads the account free. A Mac with no row pushes nothing.
///
/// NEGATIVE CONTROL: push only where the touch wrote (`quiet`'s outcome `Quieted` only for
/// `row.is_some()`), and the second off is `(Looked, None)`: the peer holds the home claim to its
/// row's `until`.
#[test]
fn an_off_whose_push_failed_is_pushed_by_the_next_quiet_or_off() {
    use std::sync::atomic::Ordering;

    let lab = Lab::new();
    let peer = Replica::new("bb02");
    let pushing = Pushing::new(&lab.table, &peer);
    assert_eq!(
        pushing.down(&lab, "first", Act::Off, NOW),
        (Touched::Looked, None),
        "no row, nothing to push"
    );
    assert_eq!(
        lab.touch("first", Act::Claim("alpha"), NOW),
        Touched::Claimed
    );
    peer.merge(&lab.table);
    assert!(!free_on(&peer, NOW));

    pushing.fail.store(true, Ordering::SeqCst);
    assert_eq!(
        pushing.down(&lab, "first", Act::Off, NOW + 1),
        (Touched::Quieted, Some(Err("no peer took it".to_owned())))
    );
    assert!(!free_on(&peer, NOW + 1), "the failed push reached nobody");

    pushing.fail.store(false, Ordering::SeqCst);
    assert_eq!(
        pushing.down(&lab, "first", Act::Off, NOW + 2),
        (Touched::Quieted, Some(Ok(())))
    );
    assert!(free_on(&peer, NOW + 2));
    assert_eq!(pushing.pushes(), 2);
}

/// An off gives up every claim though a record a look or a claim needs cannot be read: the
/// activity lock busy, the activity book corrupt or unsavable, the roster malformed, the logins
/// unreadable. Each of those refuses a claim (and a look), but no record keeps a claim through an
/// off, so none refuses it, and the claim does not stand to the row's `until`.
///
/// NEGATIVE CONTROL: drop the `Err(_) if off` arms in `touch_reading` and the `Act::Off`
/// exception in `touch_using`'s write, and every case is `Unrecorded` with the claim standing.
#[test]
fn an_off_gives_up_every_claim_whatever_record_cannot_be_read() {
    let cases = ["busy", "corrupt", "unsavable", "roster", "logins"];
    let outcomes = cases.map(|case| {
        let lab = Lab::new();
        assert_eq!(
            lab.touch("first", Act::Claim("alpha"), NOW),
            Touched::Claimed,
            "{case}"
        );
        let mut held = None;
        match case {
            "busy" => held = Some(Held::take(lab.root.path()).unwrap()),
            "corrupt" => std::fs::write(
                lab.root.path().join(".aterm/claim-activity").join(FILE),
                b"{",
            )
            .unwrap(),
            "unsavable" => {
                let active = Held::take(lab.root.path()).unwrap();
                // A directory at the output path forces the atomic rename to fail.
                std::fs::remove_file(active.dir.join(FILE)).unwrap();
                std::fs::create_dir(active.dir.join(FILE)).unwrap();
                held = Some(active);
            }
            "roster" => std::fs::write(&lab.roster, "[[account").unwrap(),
            _ => {
                std::fs::write(lab.state("first").logins_file(), "retained invalid record").unwrap()
            }
        }
        let touched = if let Some(active) = held.as_mut().filter(|_| case == "unsavable") {
            let mut state = lab.state("first").load(NOW).unwrap().0;
            touch_using(
                &lab.table,
                lab.root.path(),
                None,
                &mut state,
                Cli::Claude,
                &[("alpha".into(), key())],
                &[],
                Act::Off,
                NOW + 1,
                Ok(Some(active)),
            )
        } else {
            lab.touch("first", Act::Off, NOW + 1)
        };
        (case, touched, lab.row().claims.is_empty())
    });
    assert_eq!(outcomes, cases.map(|case| (case, Touched::Quieted, true)));
}

#[test]
fn legacy_claims_get_one_grace_period_without_perpetual_reseeding() {
    use super::super::Table as _;
    let lab = Lab::new();
    let row = claimed(
        None,
        &Mine {
            machine: "aa01".into(),
            label: "m1".into(),
        },
        &key(),
        Kind::Lease,
        |_| false,
        NOW - SESSION_FRESH_S,
    )
    .unwrap();
    lab.table.write(lab.root.path(), &row).unwrap();
    assert_eq!(lab.touch("new", Act::Look, NOW), Touched::Looked);
    assert!(lab.row().claim(&key()).is_some());
    assert_eq!(
        lab.touch("new", Act::Look, NOW + SESSION_FRESH_S - 1),
        Touched::Looked
    );
    assert!(lab.row().claim(&key()).is_some());
    assert_eq!(
        lab.touch("new", Act::Look, NOW + SESSION_FRESH_S),
        Touched::Looked
    );
    assert!(lab.row().claims.is_empty());
}

#[test]
fn corrupt_or_linked_activity_is_not_an_empty_observation() {
    use std::os::unix::fs::symlink;
    for linked in [false, true] {
        let lab = Lab::new();
        assert_eq!(
            lab.touch("first", Act::Claim("alpha"), NOW),
            Touched::Claimed
        );
        let before = lab.row();
        let file = lab.root.path().join(".aterm/claim-activity").join(FILE);
        if linked {
            let target = lab.root.path().join("keep");
            std::fs::rename(&file, &target).unwrap();
            symlink(&target, &file).unwrap();
        } else {
            std::fs::write(&file, b"{").unwrap();
        }
        let bytes = std::fs::read(&file).unwrap();
        assert!(matches!(
            lab.touch("second", Act::Claim("beta"), NOW + 1),
            Touched::Unrecorded(_)
        ));
        assert_eq!(lab.row(), before);
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
        assert!(lab.state("second").load(NOW + 1).unwrap().0.unrecorded);
    }
}

fn peer_claim(lab: &Lab, until: i64) {
    use super::super::Table as _;
    let peer = Replica::new("bb02");
    let mut row = claimed(
        None,
        &Mine {
            machine: "bb02".into(),
            label: "m2".into(),
        },
        &key(),
        Kind::Home,
        |_| false,
        NOW - 100,
    )
    .unwrap();
    row.until = until;
    peer.write(lab.root.path(), &row).unwrap();
    lab.table.merge(&peer);
    lab.state("second")
        .transact(NOW, &mut None, |state| {
            state.swarm_label = Some("m1".into());
            state.home_accounts.insert("alpha".into());
            state.note_lease("alpha", "m1", NOW + super::super::HORIZON_S, NOW);
            Ok(Vec::new())
        })
        .unwrap();
}

fn sees_peer(lab: &Lab) {
    let state = lab.state("second").load(NOW).unwrap().0;
    assert!(matches!(
        state.lease_verdict("alpha", NOW),
        Err(accounts::WhyNot::LeasedElsewhere { .. })
    ));
    assert_eq!(state.leases["alpha"].holder, "m2");
}

#[test]
fn busy_or_corrupt_activity_still_settles_the_senior_peer_and_refuses_home_launch() {
    for corrupt in [false, true] {
        let lab = Lab::new();
        peer_claim(&lab, NOW + super::super::HORIZON_S);
        let mut held = Some(Held::take(lab.root.path()).unwrap());
        if corrupt {
            let file = held.as_ref().unwrap().dir.join(FILE);
            drop(held.take());
            crate::test_config::write(file, b"{").unwrap();
        }
        assert!(matches!(
            lab.touch("second", Act::Look, NOW),
            Touched::Unrecorded(_)
        ));
        sees_peer(&lab);
        assert!(matches!(
            lab.touch("second", Act::Claim("alpha"), NOW),
            Touched::HeldElsewhere(_)
        ));
        sees_peer(&lab);
        assert!(lab.table.row("aa01").is_none());
    }
}

#[test]
fn failed_save_still_observes_a_live_peer() {
    let lab = Lab::new();
    peer_claim(&lab, NOW + super::super::HORIZON_S);
    let mut active = Held::take(lab.root.path()).unwrap();
    std::fs::create_dir(active.dir.join(FILE)).unwrap();
    let roster = accounts::Roster::load_for(Cli::Claude, &lab.roster).unwrap();
    let mut state = lab.state("second").load(NOW).unwrap().0;
    let result = touch_using(
        &lab.table,
        lab.root.path(),
        None,
        &mut state,
        Cli::Claude,
        &super::super::keys(Cli::Claude, &roster),
        &[],
        Act::Claim("alpha"),
        NOW,
        Ok(Some(&mut active)),
    );
    assert!(matches!(result, Touched::HeldElsewhere(_)));
    assert!(matches!(
        state.lease_verdict("alpha", NOW),
        Err(accounts::WhyNot::LeasedElsewhere { .. })
    ));
    assert!(state.unrecorded);
    assert!(lab.table.row("aa01").is_none());
}

#[test]
fn local_clock_floor_never_expires_a_peer_and_large_skew_refuses_publication() {
    for gap in [2, 48 * 60 * 60] {
        let lab = Lab::new();
        assert_eq!(lab.touch("first", Act::Look, NOW + gap), Touched::Looked);
        peer_claim(&lab, NOW + 1);
        assert!(matches!(
            lab.touch("second", Act::Claim("alpha"), NOW),
            Touched::HeldElsewhere(_)
        ));
        sees_peer(&lab);
        if gap > accounts::STAMP_SKEW_S {
            assert!(matches!(
                lab.touch("second", Act::Claim("beta"), NOW),
                Touched::Unrecorded(_)
            ));
            assert!(lab.table.row("aa01").is_none());
        }
    }
}

#[test]
fn a_known_unwritable_table_does_not_record_unused_candidates() {
    use super::super::{Table, Tabled};
    struct ReadOnly<'a>(&'a Replica);
    impl Table for ReadOnly<'_> {
        fn read(&self, home: &Path) -> Result<Tabled, String> {
            let mut read = self.0.read(home)?;
            read.writable = Err("read only".into());
            Ok(read)
        }
        fn write(&self, _: &Path, _: &Row) -> Result<(), String> {
            panic!("read only")
        }
    }
    let lab = Lab::new();
    let read_only = ReadOnly(&lab.table);
    let table: &dyn Table = &read_only;
    let result = touch_on(
        table,
        lab.root.path(),
        &lab.root.path().join("first"),
        Some(&lab.roster),
        Cli::Claude,
        Act::Claim("alpha"),
        NOW,
    );
    assert!(matches!(result, Touched::Unrecorded(_)), "{result:?}");
    assert!(
        lab.state("first")
            .load(NOW)
            .unwrap()
            .0
            .claimed_at
            .is_empty()
    );
    assert!(!Held::take(lab.root.path()).unwrap().live(&key(), NOW));
}

#[test]
fn failed_activity_save_cannot_publish_a_claim() {
    let lab = Lab::new();
    let mut active = Held::take(lab.root.path()).unwrap();
    // A directory at the output path forces the atomic rename to fail.
    std::fs::create_dir(active.dir.join(FILE)).unwrap();
    let mut state = accounts::State::default();
    let result = touch_using(
        &lab.table,
        lab.root.path(),
        None,
        &mut state,
        Cli::Claude,
        &[("alpha".into(), key())],
        &[],
        Act::Claim("alpha"),
        NOW,
        Ok(Some(&mut active)),
    );
    assert!(matches!(result, Touched::Unrecorded(_)), "{result:?}");
    assert!(lab.table.row("aa01").is_none());
    assert!(state.unrecorded);
}

#[test]
fn a_second_local_writer_cannot_pass_the_shared_activity_lock() {
    let lab = Lab::new();
    let held = Held::take(lab.root.path()).unwrap();
    assert!(matches!(
        lab.touch("second", Act::Claim("alpha"), NOW),
        Touched::Unrecorded(why) if why.contains("busy")
    ));
    assert!(lab.table.row("aa01").is_none());
    drop(held);
    assert_eq!(
        lab.touch("second", Act::Claim("alpha"), NOW),
        Touched::Claimed
    );
}

#[test]
fn malformed_activity_cannot_invent_an_unbounded_deadline() {
    for until in [NOW, NOW - 1, NOW + SESSION_FRESH_S + 1, i64::MAX] {
        let malformed = json!({
            "version": 1, "machine": "aa01", "at": NOW,
            "entries": [{"pool":"claude", "email":"shared@example.test", "until":until}]
        });
        assert!(Book::decode(&aterm_json::to_vec(&malformed).unwrap()).is_err());
    }
}

#[test]
fn delayed_observers_do_not_rewind_expiry_or_gain_older_seniority() {
    let lab = Lab::new();
    assert_eq!(
        lab.touch("first", Act::Claim("alpha"), NOW),
        Touched::Claimed
    );
    let before = lab.row();
    let file = lab.root.path().join(".aterm/claim-activity").join(FILE);
    let bytes = std::fs::read(&file).unwrap();
    assert_eq!(lab.touch("second", Act::Look, NOW - 1), Touched::Looked);
    assert_eq!(lab.row(), before);
    assert_eq!(std::fs::read(file).unwrap(), bytes);
    assert_eq!(
        lab.touch("second", Act::Claim("beta"), NOW - 100),
        Touched::Claimed
    );
    assert!(lab.row().claim(&key()).is_some());
    let other = LeaseKey::new("claude", "other@example.test").unwrap();
    assert_eq!(lab.row().claim(&other).unwrap().since, NOW);
    assert_eq!(
        lab.state("second").load(NOW).unwrap().0.claimed_at["beta"],
        NOW
    );
}

#[test]
fn activity_uses_byte_budget_without_evicting_fresh_evidence() {
    let mut book = Book {
        machine: "aa01".into(),
        at: NOW,
        entries: BTreeMap::new(),
    };
    for i in 0..4097 {
        book.entries.insert(
            LeaseKey::new("codex", &format!("a{i}@example.test")).unwrap(),
            NOW + 1,
        );
    }
    book.note(key(), NOW + 2).unwrap();
    assert_eq!(Book::decode(&book.encode().unwrap()).unwrap(), book);
    book.machine = "x".repeat(MAX_BYTES);
    let before = book.clone();
    assert!(
        book.note(
            LeaseKey::new("codex", "extra@example.test").unwrap(),
            NOW + 3
        )
        .is_err()
    );
    assert_eq!(book, before);
}

#[test]
fn a_lost_publication_reply_still_has_durable_evidence_before_the_write() {
    use super::super::{Table, Tabled};
    struct LostReply<'a> {
        lab: &'a Lab,
    }
    impl Table for LostReply<'_> {
        fn read(&self, home: &Path) -> Result<Tabled, String> {
            self.lab.table.read(home)
        }
        fn write(&self, home: &Path, row: &Row) -> Result<(), String> {
            let bytes = std::fs::read(home.join(".aterm/claim-activity").join(FILE)).unwrap();
            assert!(Book::decode(&bytes).unwrap().live(&key(), NOW));
            self.lab.table.write(home, row)?;
            Err("lost reply after publication".into())
        }
    }
    let lab = Lab::new();
    let result = touch_on(
        &LostReply { lab: &lab },
        lab.root.path(),
        &lab.root.path().join("first"),
        Some(&lab.roster),
        Cli::Claude,
        Act::Claim("alpha"),
        NOW,
    );
    assert!(matches!(result, Touched::Unrecorded(_)));
    assert_eq!(lab.touch("second", Act::Look, NOW + 1), Touched::Looked);
    assert_eq!(lab.row().claim(&key()).unwrap().since, NOW);
}

pub(crate) fn activity_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut active = Held::take(root.path()).unwrap();
    active.book = Some(Book {
        machine: "aa01".into(),
        at: state["now"],
        entries: if state["until"] == 0 {
            BTreeMap::new()
        } else {
            BTreeMap::from([(key(), state["until"])])
        },
    });
    let mut output = state.clone();
    match action {
        "ObserveA" => {
            active.book.as_mut().unwrap().note(key(), 1).unwrap();
            output.insert("a", 1);
        }
        "ObserveB" => {
            active.book.as_mut().unwrap().note(key(), 2).unwrap();
            output.insert("b", 1);
        }
        "Empty" | "Tick" | "Delayed" => {
            if action == "Tick" {
                output.insert("now", state["now"] + 1);
            } else if action == "Empty" {
                output.insert("empty", 1);
            } else {
                output.insert("delayed", 1);
            }
            let now = active
                .observe(
                    "aa01",
                    None,
                    &accounts::State::default(),
                    &[],
                    if action == "Delayed" {
                        0
                    } else {
                        output["now"]
                    },
                )
                .unwrap();
            output.insert("now", now);
        }
        _ => panic!("unknown activity action {action}"),
    }
    active.save().unwrap();
    drop(active);
    let active = Held::take(root.path()).unwrap();
    let book = active.book.unwrap();
    output.insert("until", book.entries.get(&key()).copied().unwrap_or(0));
    output.insert("live", i64::from(book.live(&key(), output["now"])));
    output
}

#[test]
fn activity_union_and_expiry_conform_and_the_old_empty_store_policy_fails() {
    let model = aterm_models_harness::account_claim_activity_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = activity_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} {action:?} {next:?}"
                );
            }
        }
    }
    let before = activity_transition(&model.init_state(), "ObserveB");
    let mut old = activity_transition(&before, "Empty");
    old.insert("until", 0);
    old.insert("live", 0);
    assert!(!model.check_invariant("NoEarlyRelease", &old));
    assert!(!model.successors("Empty", &before).contains(&old));
}

pub(crate) fn observation_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    use super::super::{Table, Tabled};
    let mut output = state.clone();
    match action {
        "Prepare" => {
            output.insert("prepared", 1);
        }
        "Fault" => {
            output.insert("fault", state["fault"] + 1);
        }
        "Peer" => {
            output.insert("peer", 1);
        }
        "Skew" => {
            output.insert("skew", state["skew"] + 1);
        }
        "OwnExpired" => {
            output.insert("own_expired", 1);
        }
        "Touch" => {
            let lab = Lab::new();
            if state["peer"] == 1 {
                peer_claim(&lab, NOW + 1);
            }
            if state["own_expired"] == 1 {
                let mut own = claimed(
                    None,
                    &Mine {
                        machine: "aa01".into(),
                        label: "m1".into(),
                    },
                    &key(),
                    Kind::Lease,
                    |_| false,
                    NOW - 200,
                )
                .unwrap();
                own.until = NOW + 1;
                lab.table.write(lab.root.path(), &own).unwrap();
            }
            struct Gate<'a> {
                table: &'a Replica,
                writable: bool,
            }
            impl Table for Gate<'_> {
                fn read(&self, home: &Path) -> Result<Tabled, String> {
                    let mut read = self.table.read(home)?;
                    if !self.writable {
                        read.writable = Err("read only".into());
                    }
                    Ok(read)
                }
                fn write(&self, home: &Path, row: &Row) -> Result<(), String> {
                    assert!(self.writable);
                    self.table.write(home, row)
                }
            }
            let mut held = Held::take(lab.root.path()).unwrap();
            held.book = Some(Book {
                machine: "aa01".into(),
                at: NOW
                    + match state["skew"] {
                        1 => 2,
                        2 => 48 * 3600,
                        _ => 0,
                    },
                entries: BTreeMap::new(),
            });
            if state["fault"] == 2 {
                std::fs::create_dir(held.dir.join(FILE)).unwrap();
            }
            let activity = if state["fault"] == 1 {
                Err("local activity unavailable".into())
            } else {
                Ok(Some(&mut held))
            };
            let table = Gate {
                table: &lab.table,
                writable: state["fault"] != 3 && state["fault"] != 4,
            };
            let mut local = accounts::State::default();
            if state["prepared"] == 1 {
                local.prepared_accounts.insert("alpha".into());
            }
            let result = touch_using(
                &table,
                lab.root.path(),
                None,
                &mut local,
                Cli::Claude,
                &[("alpha".into(), key())],
                &[],
                Act::Claim("alpha"),
                NOW,
                activity,
            );
            if state["prepared"] == 1 {
                assert_eq!(result, Touched::PreparationOnly);
            }
            output.insert("phase", 1);
            let peer_leased = local
                .leases
                .get("alpha")
                .is_some_and(|lease| lease.until > NOW && !local.owns_swarm_label(&lease.holder));
            output.insert("observed", i64::from(peer_leased));
            output.insert("accepted", i64::from(matches!(result, Touched::Claimed)));
            output.insert("intent", i64::from(local.claimed_at.contains_key("alpha")));
        }
        _ => panic!("unknown observation action {action}"),
    }
    output
}

#[test]
fn failed_publications_preserve_peer_observations_and_conform() {
    let model = aterm_models_harness::account_claim_observation_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = observation_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} {action:?} {next:?}"
                );
            }
        }
    }
    let with_peer = observation_transition(&model.init_state(), "Peer");
    let with_fault = observation_transition(&with_peer, "Fault");
    let mut old = observation_transition(&with_fault, "Touch");
    old.insert("observed", 0);
    assert!(!model.check_invariant("PeerObserved", &old));
    assert!(!model.successors("Touch", &with_fault).contains(&old));
    let delayed = observation_transition(&with_peer, "Skew");
    let expired = observation_transition(&delayed, "OwnExpired");
    let mut revived = observation_transition(&expired, "Touch");
    revived.insert("accepted", 1);
    revived.insert("intent", 1);
    assert!(!model.check_invariant("PeerBlocksNewGrant", &revived));
    assert!(!model.successors("Touch", &expired).contains(&revived));
    let held = observation_transition(&model.init_state(), "Prepare");
    let mut forbidden = observation_transition(&held, "Touch");
    forbidden.insert("accepted", 1);
    forbidden.insert("intent", 1);
    assert!(!model.check_invariant("PreparedCannotClaim", &forbidden));
    assert!(!model.successors("Touch", &held).contains(&forbidden));
    let held_peer = observation_transition(&held, "Peer");
    let observed = observation_transition(&held_peer, "Touch");
    assert_eq!(
        (
            observed["observed"],
            observed["accepted"],
            observed["intent"]
        ),
        (1, 0, 0)
    );
}
