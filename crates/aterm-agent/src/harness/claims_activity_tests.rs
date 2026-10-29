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
    assert_eq!(lab.touch("second", Act::Quiet, NOW + 1), Touched::Looked);
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
            std::fs::write(file, b"{").unwrap();
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
fn a_known_unwritable_or_full_table_does_not_record_unused_candidates() {
    use super::super::{Claim, MAX_CLAIMS, Table, Tabled};
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
    for full in [false, true] {
        let lab = Lab::new();
        if full {
            lab.table
                .write(
                    lab.root.path(),
                    &Row {
                        machine: "aa01".into(),
                        label: "m1".into(),
                        counter: 1,
                        until: NOW + super::super::HORIZON_S,
                        claims: (0..MAX_CLAIMS)
                            .map(|i| Claim {
                                key: LeaseKey::new("claude", &format!("busy{i}@example.test"))
                                    .unwrap(),
                                kind: Kind::Lease,
                                since: NOW,
                            })
                            .collect(),
                    },
                )
                .unwrap();
        }
        let read_only = ReadOnly(&lab.table);
        let table: &dyn Table = if full { &lab.table } else { &read_only };
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
fn full_activity_refuses_new_keys_without_evicting_fresh_evidence() {
    let mut book = Book {
        machine: "aa01".into(),
        at: NOW,
        entries: BTreeMap::new(),
    };
    for i in 0..MAX_ENTRIES {
        book.note(
            LeaseKey::new("claude", &format!("a{i}@example.test")).unwrap(),
            NOW + 1,
        )
        .unwrap();
    }
    let before = book.clone();
    assert!(book.note(key(), NOW + 2).is_err());
    assert_eq!(book, before);
    let first = book.entries.keys().next().unwrap().clone();
    book.note(first.clone(), NOW + 3).unwrap();
    assert_eq!(
        book.entries[&first],
        NOW + 3,
        "full books still refresh existing evidence"
    );
    assert_eq!(Book::decode(&book.encode().unwrap()).unwrap(), book);
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
    use super::super::{Claim, HORIZON_S, MAX_CLAIMS, Table, Tabled};
    let mut output = state.clone();
    match action {
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
            if state["fault"] == 4 {
                lab.table
                    .write(
                        lab.root.path(),
                        &Row {
                            machine: "aa01".into(),
                            label: "m1".into(),
                            counter: 1,
                            until: NOW + HORIZON_S,
                            claims: (0..MAX_CLAIMS)
                                .map(|i| Claim {
                                    key: LeaseKey::new("claude", &format!("busy{i}@example.test"))
                                        .unwrap(),
                                    kind: Kind::Lease,
                                    since: NOW,
                                })
                                .collect(),
                        },
                    )
                    .unwrap();
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
                entries: if state["fault"] == 4 {
                    lab.table
                        .row("aa01")
                        .unwrap()
                        .claims
                        .into_iter()
                        .map(|claim| (claim.key, NOW + IDLE_S))
                        .collect()
                } else {
                    BTreeMap::new()
                },
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
                writable: state["fault"] != 3,
            };
            let mut local = accounts::State::default();
            let result = touch_using(
                &table,
                lab.root.path(),
                None,
                &mut local,
                Cli::Claude,
                &[("alpha".into(), key())],
                Act::Claim("alpha"),
                NOW,
                activity,
            );
            output.insert("phase", 1);
            output.insert(
                "observed",
                i64::from(matches!(
                    local.lease_verdict("alpha", NOW),
                    Err(accounts::WhyNot::LeasedElsewhere { .. })
                )),
            );
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
}
