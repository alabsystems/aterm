// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for account claims with no broker: Macs in a lab, each with its own replica of the
//! claims table, split and healed by what their replicas exchange and what each one hears.

use std::collections::BTreeSet;
use std::sync::Mutex;

use super::*;
use crate::harness::accounts::{Advice, Class, Me, Mode, Point, State, WhyNot};

const NOW: i64 = 1_790_000_000;
const MIN: i64 = 60;
const HOUR: i64 = 3600;

/// One Mac's replica of the claims table.
pub(crate) struct Replica {
    me: String,
    book: Mutex<Book>,
    writable: Result<(), String>,
    /// Why each write is refused at its commit though the read says it may write, as a full
    /// table refuses it.
    refused: Option<String>,
}

impl Replica {
    /// The replica of the Mac whose Commons machine is `me`, holding no rows.
    pub(crate) fn new(me: &str) -> Self {
        Self {
            me: me.to_owned(),
            book: Mutex::new(Book::default()),
            writable: Ok(()),
            refused: None,
        }
    }

    /// [`Self::new`], refusing every write at its commit with `why`.
    pub(crate) fn refusing(me: &str, why: &str) -> Self {
        Self {
            refused: Some(why.to_owned()),
            ..Self::new(me)
        }
    }

    /// `machine`'s row as this replica holds it.
    pub(crate) fn row(&self, machine: &str) -> Option<Row> {
        self.book.lock().expect("replica").row(machine).cloned()
    }

    /// Take every row `from` holds, as the Commons exchange does.
    pub(crate) fn merge(&self, from: &Self) {
        let rows = from.book.lock().expect("replica").clone();
        self.book.lock().expect("replica").merge(&rows);
    }
}

impl Table for Replica {
    fn read(&self, _: &std::path::Path) -> Result<Tabled, String> {
        Ok(Tabled {
            me: self.me.clone(),
            book: self.book.lock().expect("replica").clone(),
            writable: self.writable.clone(),
        })
    }

    fn write(&self, _: &std::path::Path, row: &Row) -> Result<(), String> {
        assert_eq!(row.machine, self.me, "a Mac writes only its own row");
        if let Some(why) = &self.refused {
            return Err(why.clone());
        }
        self.book.lock().expect("replica").insert(row.clone());
        Ok(())
    }
}

/// A Mac in the lab: its replica, its accounts store's state, and when it last heard each peer.
struct Mac {
    table: Replica,
    state: State,
    heard: BTreeMap<String, i64>,
}

impl Mac {
    fn new(machine: &str, label: &str) -> Self {
        Self {
            table: Replica::new(machine),
            state: State {
                machine: format!("store-{label}"),
                swarm_label: Some(label.to_owned()),
                ..State::default()
            },
            heard: BTreeMap::new(),
        }
    }

    fn label(&self) -> &str {
        self.state.swarm_label.as_deref().unwrap_or_default()
    }

    /// This Mac's session on `account` runs at `now`, as the harness notes it.
    fn runs(&mut self, account: &str, now: i64) {
        let sid = format!("s-{account}");
        accounts::note_session(&mut self.state, &sid, account, None, now);
    }

    fn hear(&mut self, other: &Self, at: i64) {
        self.heard.insert(other.table.me.clone(), at);
    }

    fn touch(&mut self, act: Act<'_>, now: i64) -> Touched {
        let presence = Presence {
            at: now,
            heard: self.heard.clone(),
        };
        touch_with(
            &self.table,
            std::path::Path::new("/nonexistent/home"),
            Some(presence),
            &mut self.state,
            Cli::Claude,
            &keys(Cli::Claude, &roster()),
            act,
            now,
        )
    }

    /// Whether this Mac's session on `account` keeps it at its next turn end.
    fn stays_on(&self, account: &str, now: i64) -> bool {
        let me = Me {
            account: Some(account),
            left: None,
            default_home: true,
            stored_login: true,
        };
        match accounts::advise(
            &roster(),
            &self.state,
            &enrolled(),
            &me,
            &Point::TurnEnd,
            Mode::Early,
            now,
        ) {
            // With no usage reading yet the session stays on its account and reads one.
            Advice::Stay(_) | Advice::Probe => true,
            Advice::Move { to, .. } => {
                assert_ne!(to, account, "a move goes somewhere else");
                false
            }
            other => panic!("{} on {account}: {other:?}", self.label()),
        }
    }
}

/// Copy every row `from` holds into `to`'s replica.
fn replicate(from: &Mac, to: &Mac) {
    to.table.merge(&from.table);
}

/// The labels of the Macs among `on` (each with a session on `account`) that keep it at their
/// next turn end.
fn users(on: &[&Mac], account: &str, now: i64) -> Vec<String> {
    on.iter()
        .filter(|mac| mac.stays_on(account, now))
        .map(|mac| mac.label().to_owned())
        .collect()
}

fn roster() -> Roster {
    let row = |label: &str| accounts::Row {
        label: label.into(),
        class: Class::Shared,
        dir: Some(format!("/tmp/{label}")),
        email: Some(format!("{label}@example.test")),
        ..accounts::Row::default()
    };
    Roster {
        rows: vec![row("pool-a"), row("pool-b")],
    }
}

fn enrolled() -> BTreeSet<String> {
    roster().rows.into_iter().map(|row| row.label).collect()
}

fn key(label: &str) -> LeaseKey {
    LeaseKey::new("claude", &format!("{label}@example.test")).expect("a key")
}

#[test]
fn a_failed_account_transaction_is_unrecorded_not_inapplicable() {
    let root = aterm_tempfile::tempdir().unwrap();
    let roster = root.path().join("accounts.toml");
    std::fs::write(
        &roster,
        "[[account]]\nlabel = 'pool-a'\nclass = 'shared'\nemail = 'pool-a@example.test'\n",
    )
    .unwrap();
    let blocked = root.path().join("state-is-a-file");
    std::fs::write(&blocked, "preserve this file").unwrap();
    let table = Replica::new("aa01");
    let result = touch_on(
        &table,
        root.path(),
        &blocked,
        Some(&roster),
        Cli::Claude,
        Act::Claim("pool-a"),
        NOW,
    );
    assert!(
        matches!(result, Touched::Unrecorded(ref why) if !why.is_empty()),
        "{result:?}"
    );
    assert!(table.row("aa01").is_none(), "no claim was written");
    assert_eq!(
        std::fs::read_to_string(&blocked).unwrap(),
        "preserve this file"
    );
}

#[test]
fn partition_longer_than_ttl_then_heal_leaves_one_user_per_account() {
    let (mut m1, mut m2) = (Mac::new("aa01", "m1"), Mac::new("bb02", "m2"));
    m1.hear(&m2, NOW);
    m2.hear(&m1, NOW);

    // m1 launches on pool-a and claims it; once its row reaches m2, m2 cannot claim the account.
    m1.runs("pool-a", NOW);
    assert_eq!(m1.touch(Act::Claim("pool-a"), NOW), Touched::Claimed);
    replicate(&m1, &m2);
    assert!(matches!(
        m2.touch(Act::Claim("pool-a"), NOW + MIN),
        Touched::HeldElsewhere(holder) if holder.label == "m1" && holder.since == NOW
    ));
    assert_eq!(
        m2.state.lease_verdict("pool-a", NOW + MIN),
        Err(WhyNot::LeasedElsewhere {
            until: NOW + HORIZON_S
        })
    );

    // The split: from NOW + MIN neither hears the other, and it outlasts the lease TTL. m1 keeps
    // pool-a; on m2's side m1's lease lapsed, so m2 claims pool-a: one user on each side.
    let lapsed = NOW + MIN + PRESENCE_TTL_S + MIN;
    m1.runs("pool-a", lapsed);
    m2.runs("pool-a", lapsed);
    assert_eq!(m1.touch(Act::Claim("pool-a"), lapsed), Touched::Held);
    assert_eq!(m2.touch(Act::Claim("pool-a"), lapsed), Touched::Claimed);
    assert_eq!(users(&[&m1], "pool-a", lapsed), ["m1"]);
    assert_eq!(users(&[&m2], "pool-a", lapsed), ["m2"]);

    // Still split, m1's row comes due and m1 writes it again: the claim keeps its `since`.
    let renewed = NOW + HORIZON_S - RENEW_WITHIN_S + HOUR;
    m1.runs("pool-a", renewed);
    m2.runs("pool-a", renewed);
    assert_eq!(m1.touch(Act::Claim("pool-a"), renewed), Touched::Claimed);
    let row = m1.table.row("aa01");
    let row = row.expect("m1's row");
    assert_eq!((row.counter, row.until), (2, renewed + HORIZON_S));
    assert_eq!(row.claim(&key("pool-a")).map(|c| c.since), Some(NOW));

    // The heal, first step: the Macs hear each other before their rows cross. m2 holds only
    // m1's row from before the split, and it still ranks m1 senior.
    let heal = renewed + MIN;
    m1.hear(&m2, heal);
    m2.hear(&m1, heal);
    m1.runs("pool-a", heal);
    m2.runs("pool-a", heal);
    m1.touch(Act::Claim("pool-a"), heal);
    m2.touch(Act::Claim("pool-a"), heal);
    assert_eq!(users(&[&m1, &m2], "pool-a", heal), ["m1"]);

    // Second step: the rows cross both ways; every Mac settles the same holder.
    replicate(&m1, &m2);
    replicate(&m2, &m1);
    assert_eq!(m1.touch(Act::Look, heal + MIN), Touched::Looked);
    assert_eq!(m2.touch(Act::Look, heal + MIN), Touched::Looked);
    assert_eq!(users(&[&m1, &m2], "pool-a", heal + MIN), ["m1"]);

    // m2 yields at its turn end: its session moves onto pool-b, and its claim on pool-b gives
    // up pool-a, which no session there runs on now.
    assert!(!m2.stays_on("pool-a", heal + MIN));
    accounts::forget_session(&mut m2.state, "s-pool-a");
    m2.runs("pool-b", heal + 2 * MIN);
    assert_eq!(
        m2.touch(Act::Claim("pool-b"), heal + 2 * MIN),
        Touched::Claimed
    );
    let row = m2.table.row("bb02");
    let claims = row.expect("m2's row").claims;
    assert_eq!(
        claims.iter().map(|c| &c.key).collect::<Vec<_>>(),
        [&key("pool-b")]
    );
    replicate(&m2, &m1);
    m1.touch(Act::Look, heal + 3 * MIN);
    let after = heal + 3 * MIN;
    assert_eq!(users(&[&m1], "pool-a", after), ["m1"]);
    assert_eq!(users(&[&m2], "pool-b", after), ["m2"]);
    assert!(!m1.stays_on("pool-b", after), "m1 sees pool-b held by m2");
}

/// After a heal, a junior whose other rows are all walled (the pool fully used, when collisions
/// happen) still yields at its turn end, under `accounts = "wall"` too: it waits, so its turn end
/// holds, until a row opens or the claims are read again. A low row is one to move to here, as
/// at a wall. Rotation off moves nothing, as the owner set it.
///
/// NEGATIVE CONTROL: in `accounts::advise_with`, drop the `claimed_elsewhere` branch (or rank the
/// rows at the turn end itself), and m2 stays on pool-a beside m1: `Stay(NoFresh)` under Early,
/// `Stay(NothingDue)` under Wall.
#[test]
fn a_junior_with_no_row_to_move_to_yields_its_turn_end_in_every_rotating_mode() {
    let (mut m1, mut m2) = (Mac::new("aa01", "m1"), Mac::new("bb02", "m2"));
    m1.hear(&m2, NOW);
    m2.hear(&m1, NOW);
    m1.runs("pool-a", NOW);
    assert_eq!(m1.touch(Act::Claim("pool-a"), NOW), Touched::Claimed);

    // The split outlasts the lease's presence life on m2's side, so m2 claims pool-a there; its
    // other row, pool-b, is walled.
    let lapsed = NOW + MIN + PRESENCE_TTL_S + MIN;
    m2.runs("pool-a", lapsed);
    assert_eq!(m2.touch(Act::Claim("pool-a"), lapsed), Touched::Claimed);
    let wall_ends = lapsed + 4 * HOUR;
    accounts::note_wall(
        &mut m2.state,
        "pool-b",
        "five_hour",
        Some(wall_ends),
        lapsed,
        lapsed,
    );

    // The heal: the Macs hear each other and the rows cross. m1's claim is the senior one.
    let heal = lapsed + HOUR;
    m1.hear(&m2, heal);
    m2.hear(&m1, heal);
    replicate(&m1, &m2);
    replicate(&m2, &m1);
    m1.runs("pool-a", heal);
    m2.runs("pool-a", heal);
    assert_eq!(m1.touch(Act::Look, heal), Touched::Looked);
    assert_eq!(m2.touch(Act::Look, heal), Touched::Looked);
    assert!(matches!(
        m2.state.lease_verdict("pool-a", heal),
        Err(WhyNot::LeasedElsewhere { .. })
    ));
    assert!(m1.stays_on("pool-a", heal), "the senior keeps it");

    let me = Me {
        account: Some("pool-a"),
        left: None,
        default_home: true,
        stored_login: true,
    };
    let advice = |state: &State, mode: Mode| {
        accounts::advise(
            &roster(),
            state,
            &enrolled(),
            &me,
            &Point::TurnEnd,
            mode,
            heal,
        )
    };
    for mode in [Mode::Early, Mode::Wall] {
        assert_eq!(
            advice(&m2.state, mode),
            Advice::Wait {
                until: heal + RECHECK_S
            },
            "{mode:?}"
        );
    }
    assert_eq!(
        advice(&m2.state, Mode::Off),
        Advice::Stay(accounts::StayWhy::Off)
    );

    // pool-b low, not walled: a turn end would not take it ahead of a wall, but a junior takes it.
    let mut low = m2.state.clone();
    low.accounts.remove("pool-b");
    accounts::note_sample(
        &mut low,
        "pool-b",
        accounts::Sample {
            session_pct: Some(accounts::LOW_SESSION_PCT + 5),
            session_reset: Some(heal + 2 * HOUR),
            at: heal,
            ..accounts::Sample::default()
        },
        heal,
    );
    for mode in [Mode::Early, Mode::Wall] {
        assert_eq!(
            advice(&low, mode),
            Advice::Move {
                to: "pool-b".to_owned(),
                ahead: true,
                rotates: true,
                takes_canary: false,
            },
            "{mode:?}"
        );
    }
}

#[test]
fn home_use_during_a_split_blocks_a_cold_claim_elsewhere() {
    let (mut m1, mut m2) = (Mac::new("aa01", "m1"), Mac::new("bb02", "m2"));
    m1.state.home_accounts = ["pool-a".to_owned()].into_iter().collect();
    m1.hear(&m2, NOW);
    m2.hear(&m1, NOW);

    // m1 uses pool-a, its home account, and leases pool-b; both claims reach m2.
    m1.runs("pool-a", NOW);
    m1.runs("pool-b", NOW);
    assert_eq!(m1.touch(Act::Claim("pool-a"), NOW), Touched::Claimed);
    assert_eq!(m1.touch(Act::Claim("pool-b"), NOW), Touched::Claimed);
    replicate(&m1, &m2);

    // The split outlasts the lease TTL on m2's side: m1's lease on pool-b lapsed there, so m2
    // may take pool-b; m1's home use of pool-a still holds, so m2's cold claim on it is refused.
    let lapsed = NOW + MIN + PRESENCE_TTL_S + MIN;
    assert!(matches!(
        m2.touch(Act::Claim("pool-a"), lapsed),
        Touched::HeldElsewhere(holder) if holder.label == "m1" && holder.kind == Kind::Home
    ));
    assert_eq!(
        m2.state.lease_verdict("pool-a", lapsed),
        Err(WhyNot::LeasedElsewhere {
            until: NOW + HORIZON_S
        })
    );
    let row = m2.table.row("bb02");
    assert_eq!(row, None, "m2 wrote no claim");
    let roster = roster();
    let picked = accounts::launch_row(&roster, &m2.state, &enrolled(), lapsed);
    assert_eq!(picked.map(|row| row.label.as_str()), Some("pool-b"));
    m2.runs("pool-b", lapsed);
    assert_eq!(m2.touch(Act::Claim("pool-b"), lapsed), Touched::Claimed);
    assert_eq!(users(&[&m1], "pool-a", lapsed), ["m1"]);
    let row = m1.table.row("aa01").expect("m1's row");
    assert_eq!(row.claim(&key("pool-a")).map(|c| c.kind), Some(Kind::Home));
    assert_eq!(row.claim(&key("pool-b")).map(|c| c.kind), Some(Kind::Lease));
}

#[test]
fn a_row_round_trips_and_an_unreadable_claim_drops_alone() {
    let me = Mine {
        machine: "aa01".into(),
        label: "m1".into(),
    };
    let row = claimed(None, &me, &key("pool-b"), Kind::Lease, |_| true, NOW).expect("a first row");
    let row = claimed(
        Some(&row),
        &me,
        &key("pool-a"),
        Kind::Home,
        |_| true,
        NOW + MIN,
    )
    .expect("a second");
    assert_eq!((row.counter, row.until), (2, NOW + MIN + HORIZON_S));
    assert_eq!(Row::from_value("aa01", &row.to_value()), Ok(row.clone()));

    // A claim of a kind this build does not know is left out; the rest of the row stands.
    let mut body = row.to_value();
    let claims = body.as_object_mut().and_then(|row| row.get_mut("claims"));
    let first = claims
        .and_then(Value::as_array_mut)
        .and_then(|c| c.first_mut());
    let first = first.and_then(Value::as_object_mut).expect("a claim");
    first.insert("kind".to_owned(), Value::from("borrow"));
    let read = Row::from_value("aa01", &body).expect("a row");
    assert_eq!(read.claims.len(), 1);
    assert_eq!(
        read.claim(&key("pool-b")).map(|c| c.kind),
        Some(Kind::Lease)
    );

    // A body with no counter is no row.
    assert!(Row::from_value("aa01", &Value::from("claims")).is_err());
    let mut bare = row.to_value();
    bare.as_object_mut().expect("object").remove("counter");
    assert!(Row::from_value("aa01", &bare).is_err());
}

#[test]
fn a_mac_writes_its_row_only_when_its_claims_change_or_come_due() {
    let me = Mine {
        machine: "aa01".into(),
        label: "m1".into(),
    };
    let all = |_: &Claim| true;
    let claim = |current: Option<&Row>, account: &str, kind: Kind, now: i64| {
        claimed(current, &me, &key(account), kind, all, now)
    };
    let first = claim(None, "pool-a", Kind::Lease, NOW).expect("a first row");
    assert_eq!(claim(Some(&first), "pool-a", Kind::Lease, NOW + HOUR), None);
    let due = NOW + HORIZON_S - RENEW_WITHIN_S + MIN;
    let again = claim(Some(&first), "pool-a", Kind::Lease, due).expect("due");
    assert_eq!(again.claim(&key("pool-a")).map(|c| c.since), Some(NOW));
    let home = claim(Some(&first), "pool-a", Kind::Home, NOW + MIN).expect("kind");
    assert_eq!(home.claim(&key("pool-a")).map(|c| c.kind), Some(Kind::Home));

    // A lapsed row starts its claims afresh.
    let late = NOW + HORIZON_S + MIN;
    let fresh = claim(Some(&first), "pool-a", Kind::Lease, late).expect("fresh");
    assert_eq!(fresh.claim(&key("pool-a")).map(|c| c.since), Some(late));
    assert_eq!(fresh.counter, 2);

    // A claim `keep` refuses goes with the next write.
    let both = claim(Some(&first), "pool-b", Kind::Lease, NOW + MIN).expect("a second claim");
    let only_a = |c: &Claim| c.key == key("pool-a");
    let next = claimed(
        Some(&both),
        &me,
        &key("pool-a"),
        Kind::Lease,
        only_a,
        NOW + 2 * MIN,
    );
    let next = next.expect("pool-b goes");
    assert_eq!(
        next.claims.iter().map(|c| &c.key).collect::<Vec<_>>(),
        [&key("pool-a")]
    );

    // Releasing drops the claim and keeps the row's life; releasing what it does not hold writes
    // nothing.
    let a = |c: &Claim| c.key == key("pool-a");
    let gone = released(Some(&first), a, NOW + MIN).expect("released");
    assert_eq!(
        (gone.counter, gone.until, gone.claims.len()),
        (2, first.until, 0)
    );
    assert_eq!(released(Some(&gone), a, NOW + MIN), None);

    // Of two rows from one Mac the higher counter stands, whichever arrives last.
    let book = Book::of([again.clone(), first.clone()]);
    assert_eq!(book.row("aa01"), Some(&again));
}

#[test]
fn a_mac_that_cannot_record_claims_keeps_to_its_home_accounts() {
    let mut m1 = Mac::new("aa01", "m1");
    m1.state.home_accounts = ["pool-a".to_owned()].into_iter().collect();
    m1.table.writable = Err("not a Commons writer".into());
    assert_eq!(
        m1.touch(Act::Claim("pool-b"), NOW),
        Touched::Unrecorded("not a Commons writer".into())
    );
    assert!(m1.state.unrecorded);
    assert_eq!(m1.state.lease_verdict("pool-a", NOW), Ok(()));
    assert_eq!(
        m1.state.lease_verdict("pool-b", NOW),
        Err(WhyNot::Unclaimed)
    );

    // Once it can write, its claim records and the account is its own.
    m1.table.writable = Ok(());
    assert_eq!(m1.touch(Act::Claim("pool-b"), NOW + MIN), Touched::Claimed);
    assert!(!m1.state.unrecorded);
    assert_eq!(m1.state.lease_verdict("pool-b", NOW + MIN), Ok(()));
}

#[test]
fn a_presence_reading_lapses_only_on_current_evidence() {
    let row = Row {
        machine: "bb02".into(),
        label: "m2".into(),
        counter: 1,
        until: NOW + HORIZON_S,
        claims: vec![Claim {
            key: key("pool-a"),
            kind: Kind::Lease,
            since: NOW,
        }],
    };
    let claim = &row.claims[0];
    let view = |at: i64, heard: Option<i64>| View {
        me: "aa01".into(),
        presence: Some(Presence {
            at,
            heard: heard.map(|h| ("bb02".to_owned(), h)).into_iter().collect(),
        }),
    };
    let late = NOW + PRESENCE_TTL_S + MIN;
    assert!(
        !view(late, Some(NOW)).live(&row, claim, late),
        "unheard past the TTL"
    );
    assert!(
        view(late, Some(late - MIN)).live(&row, claim, late),
        "heard"
    );
    assert!(
        view(late, None).live(&row, claim, late),
        "never heard is no evidence"
    );
    let stale = late + 2 * PRESENCE_CURRENT_S;
    assert!(
        view(late, Some(NOW)).live(&row, claim, stale),
        "a stale reading"
    );
    assert!(
        view(late + 1, Some(NOW)).live(&row, claim, late),
        "a future observation cannot establish that a peer is absent"
    );
    let none = View {
        me: "aa01".into(),
        presence: None,
    };
    assert!(none.live(&row, claim, late));
    assert!(!none.live(&row, claim, row.until), "the row lapsed");
}

#[test]
fn a_future_presence_read_cannot_make_a_peer_lease_claimable() {
    let (mut m1, mut m2) = (Mac::new("aa01", "m1"), Mac::new("bb02", "m2"));
    m1.runs("pool-a", NOW);
    assert_eq!(m1.touch(Act::Claim("pool-a"), NOW), Touched::Claimed);
    replicate(&m1, &m2);
    m2.hear(&m1, NOW);
    let later = NOW + PRESENCE_TTL_S + MIN;
    m2.runs("pool-a", later);
    let outcome = touch_with(
        &m2.table,
        std::path::Path::new("/nonexistent/home"),
        Some(Presence {
            at: later + 1,
            heard: m2.heard.clone(),
        }),
        &mut m2.state,
        Cli::Claude,
        &keys(Cli::Claude, &roster()),
        Act::Claim("pool-a"),
        later,
    );
    assert!(
        matches!(outcome, Touched::HeldElsewhere(ref holder) if holder.label == "m1"),
        "a future observation freed another Mac's live lease: {outcome:?}"
    );
    assert!(
        m2.table.row("bb02").is_none(),
        "a competing claim was written"
    );
    assert_eq!(
        m2.state.lease_verdict("pool-a", later),
        Err(WhyNot::LeasedElsewhere {
            until: NOW + HORIZON_S
        })
    );

    // A subsequent current reading still frees a lease after genuine absence.
    assert_eq!(m2.touch(Act::Claim("pool-a"), later), Touched::Claimed);
}

#[test]
fn a_claim_no_session_here_ran_on_for_an_hour_goes_at_the_next_look() {
    let mut m1 = Mac::new("aa01", "m1");
    m1.runs("pool-a", NOW);
    assert_eq!(m1.touch(Act::Claim("pool-a"), NOW), Touched::Claimed);
    assert_eq!(m1.touch(Act::Claim("pool-b"), NOW), Touched::Claimed);
    // A session runs on pool-a past the hour; none on pool-b.
    let later = NOW + IDLE_S + MIN;
    m1.runs("pool-a", later - MIN);
    assert_eq!(m1.touch(Act::Look, later), Touched::Looked);
    let row = m1.table.row("aa01");
    let row = row.expect("m1's row");
    assert_eq!(row.counter, 3);
    assert_eq!(
        row.claims.iter().map(|c| &c.key).collect::<Vec<_>>(),
        [&key("pool-a")]
    );
    assert_eq!(m1.state.lease_verdict("pool-b", later), Ok(()));
    assert!(
        !m1.state.leases.contains_key("pool-b"),
        "nobody holds pool-b"
    );

    // Claude Code's store records no use of a claim in another CLI's realm, so that claim lasts
    // the idle life from its since and no Claude write keeps it past that.
    let codex = LeaseKey::new("codex", "pool-b@example.test").expect("a key");
    let other = Claim {
        key: codex,
        kind: Kind::Lease,
        since: NOW,
    };
    let claude = keys(Cli::Claude, &roster());
    assert!(in_use(Cli::Claude, &m1.state, &claude, &other, NOW + MIN));
    assert!(!in_use(Cli::Claude, &m1.state, &claude, &other, later));
    assert!(!in_use(
        Cli::Codex,
        &m1.state,
        &keys(Cli::Codex, &roster()),
        &other,
        later
    ));
}

#[test]
fn a_running_session_keeps_its_claim_and_its_seniority_past_the_hour() {
    let mut m1 = Mac::new("aa01", "m1");
    // A session launches on pool-a. The harness notes it once, and its row then stands while the
    // session's usage tap claims pool-a every few minutes through turns longer than an hour.
    m1.runs("pool-a", NOW);
    assert_eq!(m1.touch(Act::Claim("pool-a"), NOW), Touched::Claimed);
    let mut at = NOW;
    while at < NOW + SESSION_FRESH_S + 10 * MIN {
        at += RECHECK_S;
        assert_eq!(m1.touch(Act::Claim("pool-a"), at), Touched::Held);
    }
    // Another session's claim on pool-b, long past pool-a's session row, keeps pool-a as it was.
    assert_eq!(m1.touch(Act::Claim("pool-b"), at + MIN), Touched::Claimed);
    let row = m1.table.row("aa01").expect("m1's row");
    assert_eq!(row.claim(&key("pool-a")).map(|c| c.since), Some(NOW));
    assert!(row.claim(&key("pool-b")).is_some());
    assert_eq!(row.counter, 2);

    // Once its taps stop, pool-a goes at the first look past the idle life.
    let quiet = at + IDLE_S + MIN;
    assert_eq!(m1.touch(Act::Look, quiet), Touched::Looked);
    let row = m1.table.row("aa01").expect("m1's row");
    assert_eq!(row.claims.len(), 0, "{row:?}");
}

#[test]
fn a_claim_past_a_full_row_is_unrecorded_not_held() {
    let table = Replica::new("aa01");
    let labels: Vec<String> = (0..=MAX_CLAIMS).map(|n| format!("pool-{n:02}")).collect();
    let keys: Vec<(String, LeaseKey)> = labels.iter().map(|l| (l.clone(), key(l))).collect();
    let mut state = State {
        swarm_label: Some("m1".into()),
        ..State::default()
    };
    let home = std::path::Path::new("/nonexistent/home");
    let mut touch = |label: &str, now: i64| {
        touch_with(
            &table,
            home,
            None,
            &mut state,
            Cli::Claude,
            &keys,
            Act::Claim(label),
            now,
        )
    };
    for label in &labels[..MAX_CLAIMS] {
        assert_eq!(touch(label, NOW), Touched::Claimed);
    }
    let last = &labels[MAX_CLAIMS];
    assert!(matches!(touch(last, NOW + MIN), Touched::Unrecorded(_)));
    assert!(state.unrecorded);
    assert_eq!(state.lease_verdict(last, NOW + MIN), Err(WhyNot::Unclaimed));
    let row = table.row("aa01").expect("m1's row");
    assert_eq!(row.claims.len(), MAX_CLAIMS);
}

/// A Mac about to sleep gives up the claims it no longer uses and keeps the rest at most
/// [`SESSION_FRESH_S`] more, so every other Mac reads a sleeping laptop's home account free then,
/// not at its row's `until` up to a day away. A short sleep keeps the claim and its seniority,
/// and the first claim after it renews the row.
///
/// NEGATIVE CONTROL: make `Act::Quiet` a plain look (`released`), and m2 still reads m1's home
/// claim on pool-a live two hours after m1 slept.
#[test]
fn a_sleeping_macs_claims_lapse_elsewhere_two_hours_after_it_slept() {
    let (mut m1, mut m2) = (Mac::new("aa01", "m1"), Mac::new("bb02", "m2"));
    m1.state.home_accounts = ["pool-a".to_owned()].into_iter().collect();
    m1.hear(&m2, NOW);
    m2.hear(&m1, NOW);
    m1.runs("pool-a", NOW);
    assert_eq!(m1.touch(Act::Claim("pool-a"), NOW), Touched::Claimed);
    assert_eq!(m1.touch(Act::Claim("pool-b"), NOW), Touched::Claimed);

    // m1's session on pool-a runs on; nothing used pool-b past the hour. Then m1 sleeps.
    let slept = NOW + IDLE_S + MIN;
    m1.runs("pool-a", slept - MIN);
    assert_eq!(m1.touch(Act::Quiet, slept), Touched::Looked);
    let row = m1.table.row("aa01").expect("m1's row");
    assert_eq!(row.until, slept + SESSION_FRESH_S);
    assert_eq!(
        row.claims
            .iter()
            .map(|c| (&c.key, c.kind))
            .collect::<Vec<_>>(),
        [(&key("pool-a"), Kind::Home)]
    );
    // Quiet again with nothing to change writes nothing.
    assert_eq!(m1.touch(Act::Quiet, slept + MIN), Touched::Looked);
    assert_eq!(m1.table.row("aa01").map(|r| r.counter), Some(row.counter));
    replicate(&m1, &m2);

    // While m1 sleeps its home use still holds pool-a on m2, until two hours after it slept.
    m2.hear(&m1, slept);
    let asleep = slept + HOUR;
    assert!(matches!(
        m2.touch(Act::Claim("pool-a"), asleep),
        Touched::HeldElsewhere(holder) if holder.kind == Kind::Home
    ));
    let lapsed = slept + SESSION_FRESH_S + MIN;
    m2.runs("pool-a", lapsed);
    assert_eq!(m2.touch(Act::Claim("pool-a"), lapsed), Touched::Claimed);

    // A short sleep: m1 wakes half an hour in and its tap renews the row, the claim's since kept.
    let mut m3 = Mac::new("cc03", "m3");
    m3.state.home_accounts = ["pool-a".to_owned()].into_iter().collect();
    m3.runs("pool-a", NOW);
    assert_eq!(m3.touch(Act::Claim("pool-a"), NOW), Touched::Claimed);
    assert_eq!(m3.touch(Act::Quiet, NOW + MIN), Touched::Looked);
    let woke = NOW + 31 * MIN;
    m3.runs("pool-a", woke);
    assert_eq!(m3.touch(Act::Claim("pool-a"), woke), Touched::Claimed);
    let row = m3.table.row("cc03").expect("m3's row");
    assert_eq!(row.until, woke + HORIZON_S);
    assert_eq!(row.claim(&key("pool-a")).map(|c| c.since), Some(NOW));
}
