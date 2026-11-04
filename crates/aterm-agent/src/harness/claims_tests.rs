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
    /// What the table says of this Mac's clock (F22).
    clock: Option<ClockRead>,
    /// The peers the table reads as skewed (F22), each one held for too, as the db's hold line
    /// covers every peer it names.
    skewed: Vec<SkewedPeer>,
    /// The peers the table holds for beyond those it names skewed (F22).
    held: Vec<SkewedPeer>,
}

impl Replica {
    /// The replica of the Mac whose Commons machine is `me`, holding no rows.
    pub(crate) fn new(me: &str) -> Self {
        Self {
            me: me.to_owned(),
            book: Mutex::new(Book::default()),
            writable: Ok(()),
            refused: None,
            clock: None,
            skewed: Vec::new(),
            held: Vec::new(),
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
            clock: self.clock.clone(),
            skewed: self.skewed.clone(),
            held: self.skewed.iter().chain(&self.held).cloned().collect(),
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
        class: Class::Pool,
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
fn preparation_only_machines_neither_claim_nor_reserve_an_idle_peers_account() {
    let mut prepared = Mac::new("aa01", "m1");
    let mut authorized = Mac::new("bb02", "m2");
    prepared.state.prepared_accounts.insert("pool-a".into());
    prepared.runs("pool-a", NOW);
    assert_eq!(
        prepared.touch(Act::Claim("pool-a"), NOW),
        Touched::PreparationOnly
    );
    assert!(prepared.table.row("aa01").is_none());
    assert!(prepared.state.claimed_at.is_empty());
    replicate(&prepared, &authorized);
    assert_eq!(
        authorized.touch(Act::Claim("pool-a"), NOW + 1),
        Touched::Claimed
    );
    replicate(&authorized, &prepared);
    assert_eq!(prepared.touch(Act::Look, NOW + 2), Touched::Looked);
    assert!(prepared.state.leases.contains_key("pool-a"));
}

#[test]
fn a_preparation_refusal_survives_an_unreadable_claim_table() {
    struct Unread;
    impl Table for Unread {
        fn read(&self, _: &std::path::Path) -> Result<Tabled, String> {
            Err("claim table unreadable".into())
        }
        fn write(&self, _: &std::path::Path, _: &Row) -> Result<(), String> {
            panic!("a refused claim cannot write");
        }
    }
    let mut state = State::default();
    state.prepared_accounts.insert("pool-a".into());
    state.home_accounts.insert("pool-a".into());
    assert_eq!(
        touch_with(
            &Unread,
            std::path::Path::new("/unused"),
            None,
            &mut state,
            Cli::Claude,
            &keys(Cli::Claude, &roster()),
            Act::Claim("pool-a"),
            NOW
        ),
        Touched::PreparationOnly
    );
    assert!(state.claimed_at.is_empty());
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

    // pool-b low, not walled: a turn end would not take it ahead of a wall, but a junior takes it,
    // and a turn end whose move is deferred holds as the wait above does.
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
                yields: Some(heal + RECHECK_S),
            },
            "{mode:?}"
        );
    }
}

/// After a heal, a junior whose Mac's `active` is another row still waiting on its canary (a
/// rotation another session there made) yields at its turn end in every rotating mode: it waits,
/// so its turn end holds, until the canary's hold ends or the claims are read again. Then it
/// takes the canary onto that row, a low one included, as at a wall. The senior keeps its own row
/// for the canary, and a turn end of its holds nothing.
///
/// NEGATIVE CONTROL: in `accounts::advise_with`, return `Stay(Canary)` from the `active` block
/// whatever the claim, and m2 stays on pool-a beside m1: `Stay(Canary)`, which a turn end reads
/// as nothing to hold. Leave the claim out of the canary's take, and m2 waits on a low pool-b no
/// session answers; leave the canary's end out of the wait, and it waits to the next look.
#[test]
fn a_junior_yields_while_another_row_waits_on_its_canary() {
    use crate::harness::accounts::{CANARY_HOLD_S, Mover, StayWhy};
    use crate::supervise::policy::turn_end::AccountAdvice;

    let (mut m1, mut m2) = (Mac::new("aa01", "m1"), Mac::new("bb02", "m2"));
    m1.hear(&m2, NOW);
    m2.hear(&m1, NOW);
    m1.runs("pool-a", NOW);
    assert_eq!(m1.touch(Act::Claim("pool-a"), NOW), Touched::Claimed);
    let lapsed = NOW + MIN + PRESENCE_TTL_S + MIN;
    m2.runs("pool-a", lapsed);
    assert_eq!(m2.touch(Act::Claim("pool-a"), lapsed), Touched::Claimed);
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

    // On each Mac another session rotated onto pool-b seven minutes before the heal, and pool-b
    // has served nothing since: its canary holds to `turned + CANARY_HOLD_S`.
    let turned = heal - 7 * MIN;
    let rotated = |mac: &Mac, at: i64| {
        let mut state = mac.state.clone();
        let mover = Mover {
            sid: "s-rotated",
            account: "pool-a",
        };
        accounts::begin_rotation(
            &mut state,
            &roster(),
            &enrolled(),
            None,
            mover,
            "pool-b",
            at,
        )
        .expect("the rotation");
        state
    };
    let (senior, junior) = (rotated(&m1, turned), rotated(&m2, turned));
    assert!(junior.canary_holds(heal));

    let me = Me {
        account: Some("pool-a"),
        left: None,
        default_home: true,
        stored_login: true,
    };
    let advice = |state: &State, point: &Point, mode: Mode, now: i64| {
        accounts::advise(&roster(), state, &enrolled(), &me, point, mode, now)
    };
    let wall = Point::Wall {
        window: "five_hour".to_owned(),
        at: None,
    };
    let clock = std::time::Instant::now();
    let at_turn_end = |advice: &Advice, state: &State, now: i64| {
        AccountAdvice::of_roster_at(advice, None, state, None, false, now, clock)
    };
    let in_secs = |s: i64| clock + std::time::Duration::from_secs(u64::try_from(s).unwrap());
    let ends = turned + CANARY_HOLD_S;
    for mode in [Mode::Early, Mode::Wall] {
        // The canary's hold ends before the claims are read again, so the junior waits to then.
        let held = advice(&junior, &Point::TurnEnd, mode, heal);
        assert_eq!(held, Advice::Wait { until: ends }, "{mode:?}");
        assert_eq!(
            at_turn_end(&held, &junior, heal),
            Some(AccountAdvice::Hold {
                until: in_secs(ends - heal)
            }),
            "{mode:?}"
        );
        // A canary just taken holds past the next look.
        let fresh = rotated(&m2, heal);
        assert_eq!(
            advice(&fresh, &Point::TurnEnd, mode, heal),
            Advice::Wait {
                until: heal + RECHECK_S
            },
            "{mode:?}"
        );

        // The senior stays for the canary, and its turn end holds nothing; at a wall it waits
        // for the canary as before.
        let stays = advice(&senior, &Point::TurnEnd, mode, heal);
        assert_eq!(stays, Advice::Stay(StayWhy::Canary), "{mode:?}");
        assert_eq!(at_turn_end(&stays, &senior, heal), None, "{mode:?}");
        let walled = advice(&senior, &wall, mode, heal);
        assert_eq!(walled, Advice::Stay(StayWhy::Canary), "{mode:?}");
        assert_eq!(
            AccountAdvice::of_roster_at(&walled, None, &senior, None, true, heal, clock),
            Some(AccountAdvice::Wait {
                until: in_secs(ends - heal)
            }),
            "{mode:?}"
        );

        // Past the hold, both take the canary onto pool-b while it is fresh: the junior's move is
        // its yield, which a turn end whose move is deferred holds to the next look for.
        let take = |yields| Advice::Move {
            to: "pool-b".to_owned(),
            ahead: true,
            rotates: false,
            takes_canary: true,
            yields,
        };
        let (onto, follows) = (take(Some(ends + RECHECK_S)), take(None));
        assert_eq!(
            advice(&junior, &Point::TurnEnd, mode, ends),
            onto,
            "{mode:?}"
        );
        assert_eq!(
            advice(&senior, &Point::TurnEnd, mode, ends),
            follows,
            "{mode:?}"
        );

        // A low pool-b: a turn end does not take it ahead of a wall, but a junior takes it.
        let low = |state: &State| {
            let mut low = state.clone();
            accounts::note_sample(
                &mut low,
                "pool-b",
                accounts::Sample {
                    session_pct: Some(accounts::LOW_SESSION_PCT + 5),
                    session_reset: Some(ends + 2 * HOUR),
                    at: ends,
                    ..accounts::Sample::default()
                },
                ends,
            );
            low
        };
        assert_eq!(
            advice(&low(&junior), &Point::TurnEnd, mode, ends),
            onto,
            "{mode:?}"
        );
        assert_eq!(
            advice(&low(&senior), &Point::TurnEnd, mode, ends),
            Advice::Stay(StayWhy::Canary),
            "{mode:?}"
        );
    }
}

/// After a heal, a junior whose own row is its Mac's `active`, still unanswered (a rotation onto
/// it during the split), moves off it at its turn end, in its canary's hold and past it: every
/// session there on that row is a junior, whose turn end holds, so none serves the turn that
/// would answer it. A rotation off an `active` this Mac may not use is admitted, a wall's on
/// another row too. The senior's same unanswered `active` still holds every rotation off it.
///
/// NEGATIVE CONTROL: in `accounts::begin_rotation`, hold a rotation off an unanswered `active`
/// whatever its claim, and the junior waits at every look, with no end, while pool-b sits unused;
/// hold none off one whatever its claim, and the senior's commit is admitted too.
#[test]
fn a_junior_on_its_own_unanswered_active_moves_off_it() {
    use crate::harness::accounts::{CANARY_HOLD_S, Lost, Mover, StayWhy};

    let (mut m1, mut m2) = (Mac::new("aa01", "m1"), Mac::new("bb02", "m2"));
    m1.hear(&m2, NOW);
    m2.hear(&m1, NOW);
    m1.runs("pool-a", NOW);
    assert_eq!(m1.touch(Act::Claim("pool-a"), NOW), Touched::Claimed);
    let lapsed = NOW + MIN + PRESENCE_TTL_S + MIN;
    m2.runs("pool-a", lapsed);
    assert_eq!(m2.touch(Act::Claim("pool-a"), lapsed), Touched::Claimed);
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

    // On each Mac a session walled on pool-c rotated onto pool-a seven minutes before the heal,
    // and pool-a has served nothing since: its canary holds to `turned + CANARY_HOLD_S`.
    let turned = heal - 7 * MIN;
    let rotated = |mac: &Mac| {
        let mut state = mac.state.clone();
        let mover = Mover {
            sid: "s-rotated",
            account: "pool-c",
        };
        accounts::begin_rotation(
            &mut state,
            &roster(),
            &enrolled(),
            None,
            mover,
            "pool-a",
            turned,
        )
        .expect("the rotation");
        state
    };
    let (senior, junior) = (rotated(&m1), rotated(&m2));
    assert!(junior.canary_holds(heal));

    let me = Me {
        account: Some("pool-a"),
        left: None,
        default_home: true,
        stored_login: true,
    };
    let advice = |state: &State, mode: Mode, now: i64| {
        accounts::advise(
            &roster(),
            state,
            &enrolled(),
            &me,
            &Point::TurnEnd,
            mode,
            now,
        )
    };
    let ends = turned + CANARY_HOLD_S;
    let late = heal + CANARY_HOLD_S + RECHECK_S;
    let looks = [
        (heal, ends),
        (ends, ends + RECHECK_S),
        (late, late + RECHECK_S),
    ];
    let moves: Vec<_> = looks
        .iter()
        .map(|&(_, until)| Advice::Move {
            to: "pool-b".to_owned(),
            ahead: true,
            rotates: true,
            takes_canary: false,
            yields: Some(until),
        })
        .collect();
    for mode in [Mode::Early, Mode::Wall] {
        let seen: Vec<_> = looks
            .iter()
            .map(|&(now, _)| advice(&junior, mode, now))
            .collect();
        assert_eq!(seen, moves, "{mode:?}");
        let stays = advice(&senior, mode, heal);
        assert_eq!(stays, Advice::Stay(StayWhy::NothingDue), "{mode:?}");
    }

    // The host's commit of the junior's move, and of a wall's on another row, is admitted; the
    // senior's is held for its canary.
    let commit = |state: &State, account: &str| {
        let mover = Mover {
            sid: "s-mover",
            account,
        };
        let active = state.active.clone();
        let mut state = state.clone();
        accounts::begin_rotation(
            &mut state,
            &roster(),
            &enrolled(),
            active.as_deref(),
            mover,
            "pool-b",
            heal,
        )
        .map(|event| (event.account, state.active))
    };
    for account in ["pool-a", "pool-c"] {
        assert_eq!(
            commit(&junior, account),
            Ok(("pool-a".to_owned(), Some("pool-b".to_owned()))),
            "{account}"
        );
        assert_eq!(
            commit(&senior, account),
            Err(Lost::Held {
                active: "pool-a".to_owned()
            }),
            "{account}"
        );
    }
}

/// The m1/m2 lab after a heal that leaves m1's claim on pool-a senior: m2's sessions there are
/// juniors ([`WhyNot::LeasedElsewhere`]).
fn healed_on_pool_a() -> (Mac, Mac, i64) {
    let (mut m1, mut m2) = (Mac::new("aa01", "m1"), Mac::new("bb02", "m2"));
    m1.hear(&m2, NOW);
    m2.hear(&m1, NOW);
    m1.runs("pool-a", NOW);
    assert_eq!(m1.touch(Act::Claim("pool-a"), NOW), Touched::Claimed);
    let lapsed = NOW + MIN + PRESENCE_TTL_S + MIN;
    m2.runs("pool-a", lapsed);
    assert_eq!(m2.touch(Act::Claim("pool-a"), lapsed), Touched::Claimed);
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
    (m1, m2, heal)
}

/// The host's commit of a rotation by `mover` (a session on that row) off `state`'s `active`
/// onto pool-b at `now`: the row it left and the `active` and pin it leaves, or why not.
fn rotated_off(
    state: &State,
    mover: &str,
    now: i64,
) -> Result<(String, Option<String>, bool), accounts::Lost> {
    let mut state = state.clone();
    let active = state.active.clone();
    let mover = accounts::Mover {
        sid: "s-mover",
        account: mover,
    };
    accounts::begin_rotation(
        &mut state,
        &roster(),
        &enrolled(),
        active.as_deref(),
        mover,
        "pool-b",
        now,
    )
    .map(|event| (event.account, state.active, state.pinned))
}

/// An `active` the owner pinned (`aterm harness accounts use`) holds every rotation off it while
/// it runs, another Mac's senior claim on it or not: the claim is no end of the pin, which only
/// `accounts use --auto` ends, as a launch under it says. So m2's juniors on its pinned pool-a
/// wait for that claim, as with no row to move to, and keep the pin, though pool-b is free; no
/// rotation erases it unannounced (a rotation clears `pinned`, and its strike as not moved puts
/// `active` back without it).
///
/// NEGATIVE CONTROL: in `accounts::begin_rotation`, let an `active` this Mac may not use hold no
/// rotation whatever its pin, and the junior is advised onto pool-b and its commit clears the
/// pin. Once `accounts use --auto` ends the pin, the same junior moves at once: the pin is what
/// holds it.
#[test]
fn a_pinned_active_another_mac_holds_keeps_its_pin() {
    use crate::harness::accounts::Lost;

    let (_, mut m2, heal) = healed_on_pool_a();
    accounts::pin(&mut m2.state, &roster(), &enrolled(), "pool-a", heal).expect("the pin");
    let me = Me {
        account: Some("pool-a"),
        left: None,
        default_home: true,
        stored_login: true,
    };
    for mode in [Mode::Early, Mode::Wall] {
        let advice = accounts::advise(
            &roster(),
            &m2.state,
            &enrolled(),
            &me,
            &Point::TurnEnd,
            mode,
            heal,
        );
        assert_eq!(
            advice,
            Advice::Wait {
                until: heal + RECHECK_S
            },
            "{mode:?}"
        );
    }
    assert_eq!(rotated_off(&m2.state, "pool-a", heal), Err(Lost::Pinned));
    assert_eq!(
        rotated_off(&m2.state, "pool-c", heal),
        Err(Lost::Held {
            active: "pool-a".to_owned()
        })
    );

    // `accounts use --auto` ends the pin, and the junior moves at once.
    let mut auto = m2.state.clone();
    assert!(accounts::unpin(&mut auto, "auto", heal).is_some());
    for mode in [Mode::Early, Mode::Wall] {
        let advice = accounts::advise(
            &roster(),
            &auto,
            &enrolled(),
            &me,
            &Point::TurnEnd,
            mode,
            heal,
        );
        assert!(
            matches!(&advice, Advice::Move { to, rotates: true, .. } if to == "pool-b"),
            "{mode:?}: {advice:?}"
        );
    }
    assert_eq!(
        rotated_off(&auto, "pool-a", heal),
        Ok(("pool-a".to_owned(), Some("pool-b".to_owned()), false))
    );
}

/// An `active` this Mac may not use holds no rotation off it, whatever else holds one: a proven,
/// fresh `active` another Mac's senior claim holds (which no session here may be moved onto) lets
/// a wall on another row rotate, and so does an unanswered one this Mac cannot record a claim on
/// ([`WhyNot::Unclaimed`]: a non-Writer, off its home rows). The same `active` this Mac may use
/// still holds that rotation.
///
/// NEGATIVE CONTROL: in `accounts::begin_rotation`, free only an unanswered `active` this Mac may
/// not use, and the wall off the proven one is refused; free only one another Mac's claim holds,
/// and the wall off the unrecorded one is refused.
#[test]
fn an_active_this_mac_may_not_use_holds_no_rotation_off_it() {
    use crate::harness::accounts::Lost;

    let held = Err(Lost::Held {
        active: "pool-a".to_owned(),
    });
    let (m1, m2, heal) = healed_on_pool_a();
    let proven = |mac: &Mac| {
        let mut state = mac.state.clone();
        state.active = Some("pool-a".to_owned());
        state.proven = true;
        state
    };
    assert_eq!(
        rotated_off(&proven(&m2), "pool-c", heal),
        Ok(("pool-a".to_owned(), Some("pool-b".to_owned()), false))
    );
    assert_eq!(rotated_off(&proven(&m1), "pool-c", heal), held);

    let unrecorded = |home: bool| {
        let mut state = State {
            swarm_label: Some("m3".to_owned()),
            unrecorded: true,
            ..State::default()
        };
        if home {
            state.home_accounts.insert("pool-a".to_owned());
        }
        let mover = accounts::Mover {
            sid: "s-rotated",
            account: "pool-c",
        };
        accounts::begin_rotation(
            &mut state,
            &roster(),
            &enrolled(),
            None,
            mover,
            "pool-a",
            NOW,
        )
        .expect("the rotation");
        state
    };
    assert_eq!(
        unrecorded(false).lease_verdict("pool-a", NOW + MIN),
        Err(WhyNot::Unclaimed)
    );
    assert_eq!(
        rotated_off(&unrecorded(false), "pool-c", NOW + MIN),
        Ok(("pool-a".to_owned(), Some("pool-b".to_owned()), false))
    );
    assert_eq!(rotated_off(&unrecorded(true), "pool-c", NOW + MIN), held);
}

/// A junior's move whose relaunch is put off (`wait:changed` for a live background job,
/// `wait:held`, `wait:no-socket`) sends no signal, so its host strikes it as not moved, and the
/// strike undoes it: a rotation's `active` goes back, a canary take's `canary` goes. Asked again,
/// the roster advises the same move, its yield with it, so the turn end that fell back holds to
/// the yield's instant and types nothing onto the account m1 holds. The point's own act still
/// makes the move, and a senior's same canary take holds nothing.
///
/// NEGATIVE CONTROL: set no `yields` in `accounts::advise_with`, or hold only on
/// `AccountAdvice::Hold` in `account_hold`, and the fallback goes on typing onto the account;
/// hold a yield whatever the act, and the point's own move waits too.
#[test]
fn a_juniors_move_the_relaunch_defers_is_advised_again_as_its_yield() {
    use crate::harness::accounts::{CANARY_HOLD_S, Mover, Strike};
    use crate::supervise::policy::turn_end::{
        AccountAdvice, AccountAt, Restart, TurnEndAction, account_hold,
    };

    let me = Me {
        account: Some("pool-a"),
        left: None,
        default_home: true,
        stored_login: true,
    };
    let advise = |state: &State| {
        accounts::advise(
            &roster(),
            state,
            &enrolled(),
            &me,
            &Point::TurnEnd,
            Mode::Early,
            NOW,
        )
    };
    let on_pool_a = |junior: bool| {
        let mut state = State {
            swarm_label: Some("m2".to_owned()),
            ..State::default()
        };
        if junior {
            state.note_lease("pool-a", "m1", NOW + HOUR, NOW - HOUR);
        }
        accounts::note_session(&mut state, "s-pool-a", "pool-a", None, NOW);
        state
    };
    // Another session here rotated onto pool-b a canary hold ago, and pool-b has served nothing.
    let after_a_canary = |junior: bool| {
        let mut state = on_pool_a(junior);
        let mover = Mover {
            sid: "s-rotated",
            account: "pool-a",
        };
        let at = NOW - CANARY_HOLD_S;
        accounts::begin_rotation(
            &mut state,
            &roster(),
            &enrolled(),
            None,
            mover,
            "pool-b",
            at,
        )
        .expect("the rotation");
        state
    };
    // The host's commit of the advised move, then the strike of the relaunch that never signalled.
    let deferred = |state: &State, advice: &Advice| {
        let mut state = state.clone();
        let Advice::Move {
            to,
            rotates,
            takes_canary,
            ..
        } = advice
        else {
            panic!("{advice:?}");
        };
        if *rotates {
            let mover = Mover {
                sid: "s-pool-a",
                account: "pool-a",
            };
            let expect = state.active.clone();
            accounts::begin_rotation(
                &mut state,
                &roster(),
                &enrolled(),
                expect.as_deref(),
                mover,
                to,
                NOW,
            )
            .expect("the move's rotation");
        } else {
            assert!(*takes_canary, "{advice:?}");
            let expect = state.canary.clone();
            accounts::take_canary(&mut state, expect.as_ref(), "s-pool-a", NOW)
                .expect("the move's canary");
        }
        let why = Strike::NotMoved("wait:changed".to_owned());
        accounts::strike(&mut state, to, "s-pool-a", &why, NOW);
        accounts::note_session(&mut state, "s-pool-a", "pool-a", Some("jobs"), NOW);
        state
    };
    let clock = std::time::Instant::now();
    let typed = TurnEndAction::Type {
        text: "keep going".to_owned(),
        rule_id: "continue@v1",
    };
    let moves = TurnEndAction::Restart {
        why: Restart::Account {
            to: "pool-b".to_owned(),
            at: AccountAt::Ahead,
        },
        rule_id: "account-rotate@v1",
        otherwise: Box::new(typed.clone()),
    };
    let look = clock + std::time::Duration::from_secs(u64::try_from(RECHECK_S).unwrap());

    for (case, state) in [
        ("rotation", on_pool_a(true)),
        ("canary take", after_a_canary(true)),
    ] {
        let first = advise(&state);
        assert!(
            matches!(&first, Advice::Move { to, yields: Some(until), .. }
                if to == "pool-b" && *until == NOW + RECHECK_S),
            "{case}: {first:?}"
        );
        let after = deferred(&state, &first);
        assert_eq!(after.active, state.active, "{case}: the strike undid it");
        assert_eq!(after.canary, None, "{case}");
        let again = advise(&after);
        assert_eq!(again, first, "{case}: the same move, its yield with it");
        let advised = AccountAdvice::of_roster_at(&again, None, &after, None, false, NOW, clock);
        assert_eq!(
            advised,
            Some(AccountAdvice::Move {
                to: "pool-b".to_owned(),
                yields: Some(look),
            }),
            "{case}"
        );
        let held = account_hold(advised.as_ref(), clock, &typed);
        assert!(
            matches!(&held, Some(TurnEndAction::WaitUntil { until, why, .. })
                if *until == look && why.starts_with("this account cannot run here")),
            "{case}: {held:?}"
        );
        assert_eq!(
            account_hold(advised.as_ref(), clock, &moves),
            None,
            "{case}: the point's own act makes the move"
        );
    }

    let senior = after_a_canary(false);
    let follows = advise(&senior);
    assert!(
        matches!(&follows, Advice::Move { to, takes_canary: true, yields: None, .. } if to == "pool-b"),
        "{follows:?}"
    );
    let advised = AccountAdvice::of_roster_at(&follows, None, &senior, None, false, NOW, clock);
    assert_eq!(account_hold(advised.as_ref(), clock, &typed), None);
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
        ..View::default()
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
        ..View::default()
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
fn large_claim_sets_round_trip_without_count_truncation() {
    for cli in CLAIMED_CLIS {
        let mine = Mine {
            machine: "aa01".into(),
            label: "m1".into(),
        };
        let mut row = None;
        for index in 0..256 {
            let key =
                LeaseKey::for_cli(cli, None, &format!("account{index}@example.test")).unwrap();
            row = claimed(row.as_ref(), &mine, &key, Kind::Lease, |_| true, NOW);
            assert_eq!(row.as_ref().unwrap().claims.len(), index + 1);
        }
        let row = row.unwrap();
        assert_eq!(
            Row::from_value(&mine.machine, &row.to_value()).unwrap(),
            row
        );
        let oversized = Mine {
            label: "x".repeat(MAX_CLAIM_BYTES),
            ..mine
        };
        assert!(
            claimed(
                Some(&row),
                &oversized,
                &key("extra"),
                Kind::Lease,
                |_| true,
                NOW
            )
            .is_none()
        );
        assert_eq!(
            Row::from_value("aa01", &row.to_value())
                .unwrap()
                .claims
                .len(),
            256
        );
    }
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
    assert_eq!(m1.touch(Act::Quiet, slept), Touched::Quieted);
    let row = m1.table.row("aa01").expect("m1's row");
    assert_eq!(row.until, slept + SESSION_FRESH_S);
    assert_eq!(
        row.claims
            .iter()
            .map(|c| (&c.key, c.kind))
            .collect::<Vec<_>>(),
        [(&key("pool-a"), Kind::Home)]
    );
    // Quiet again with nothing to change writes nothing; the row stands to be pushed again.
    assert_eq!(m1.touch(Act::Quiet, slept + MIN), Touched::Quieted);
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
    assert_eq!(m3.touch(Act::Quiet, NOW + MIN), Touched::Quieted);
    let woke = NOW + 31 * MIN;
    m3.runs("pool-a", woke);
    assert_eq!(m3.touch(Act::Claim("pool-a"), woke), Touched::Claimed);
    let row = m3.table.row("cc03").expect("m3's row");
    assert_eq!(row.until, woke + HORIZON_S);
    assert_eq!(row.claim(&key("pool-a")).map(|c| c.since), Some(NOW));
}

/// A Mac that shuts down, restarts or logs out gives up every claim, its home use among them, so
/// every other Mac reads its accounts free at once, not at its row's `until` up to a day away,
/// though a home claim never lapses by silence. With nothing left to give up it writes nothing,
/// and its row still stands to be pushed.
///
/// NEGATIVE CONTROL: map `Act::Off` to `Act::Look`'s release (`released(.., |c| !used(c), ..)`),
/// and the touch writes nothing (`Looked`, not `Quieted`): m1's row keeps its home claim on
/// pool-a, which m2 reads live to its `until`.
#[test]
fn a_mac_that_powers_off_frees_its_accounts_on_every_other_mac() {
    let (mut m1, mut m2) = (Mac::new("aa01", "m1"), Mac::new("bb02", "m2"));
    m1.state.home_accounts = ["pool-a".to_owned()].into_iter().collect();
    m1.hear(&m2, NOW);
    m2.hear(&m1, NOW);
    m1.runs("pool-a", NOW);
    m1.runs("pool-b", NOW);
    assert_eq!(m1.touch(Act::Claim("pool-a"), NOW), Touched::Claimed);
    assert_eq!(m1.touch(Act::Claim("pool-b"), NOW), Touched::Claimed);
    replicate(&m1, &m2);
    let view = View {
        me: "bb02".into(),
        presence: None,
        ..View::default()
    };
    let senior = |mac: &Mac, account: &str, now: i64| {
        let book = mac.table.book.lock().expect("replica").clone();
        book.senior(&key(account), &view, now)
    };
    assert_eq!(
        senior(&m2, "pool-a", NOW + MIN).map(|holder| holder.kind),
        Some(Kind::Home)
    );

    // m1 shuts down while its sessions still use both accounts.
    let off = NOW + MIN;
    assert_eq!(m1.touch(Act::Off, off), Touched::Quieted);
    let row = m1.table.row("aa01").expect("m1's row");
    assert!(row.claims.is_empty(), "{row:?}");
    assert_eq!(row.until, NOW + HORIZON_S, "the row keeps its life");
    replicate(&m1, &m2);
    for account in ["pool-a", "pool-b"] {
        assert_eq!(senior(&m2, account, off), None, "{account}");
    }
    m2.runs("pool-a", off);
    assert_eq!(m2.touch(Act::Claim("pool-a"), off), Touched::Claimed);

    // Down again with nothing held: nothing to write; the row stands to be pushed again.
    assert_eq!(m1.touch(Act::Off, off + MIN), Touched::Quieted);
    assert_eq!(
        m1.table.row("aa01").map(|row| row.counter),
        Some(row.counter)
    );
}

#[test]
fn keys_prefers_expected_identity_email_for_default_and_alias_rows() {
    let roster = Roster {
        rows: vec![
            accounts::Row {
                label: "default".into(),
                identity_email: Some("canonical@example.test".into()),
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "alias-row".into(),
                dir: Some("/tmp/alias".into()),
                email: Some("alias@example.test".into()),
                identity_email: Some("canonical@example.test".into()),
                ..accounts::Row::default()
            },
        ],
    };
    let got = keys(Cli::Claude, &roster);
    let expected = LeaseKey::for_cli(Cli::Claude, None, "canonical@example.test").unwrap();
    assert_eq!(
        got,
        vec![
            ("default".into(), expected.clone()),
            ("alias-row".into(), expected),
        ]
    );
}

// F22: claims under a wrong clock (docs/DESIGN-fluid-swarm-2026-10-28.md).

/// What the table says of a clock its peers read two days ahead of theirs, five of six.
fn clock_off() -> ClockRead {
    ClockRead {
        standing: accounts::ClockStanding::Off { ahead: true },
        by_s: 2 * 24 * HOUR,
        against: 5,
        of: 6,
    }
}

/// What the table says of a clock its peers are split over, one each way.
fn clock_split() -> ClockRead {
    ClockRead {
        standing: accounts::ClockStanding::Split,
        by_s: 0,
        against: 1,
        of: 2,
    }
}

/// A peer read as skewed: Commons machine `machine`, swarm label `label`, its holds lasting to
/// `until`.
fn skewed_peer(machine: &str, label: &str, until: i64) -> SkewedPeer {
    SkewedPeer {
        machine: machine.to_owned(),
        labels: vec![label.to_owned()],
        until,
        ahead_s: 0,
    }
}

/// The db's refusal of a row from a Mac its peers read off.
const OFF_REFUSAL: &str = "this Mac's clock reads at least 2 d ahead of 5 of the 6 Macs it hears; \
                           it writes no account claim until the clock agrees";

impl Mac {
    /// This Mac's peers read its clock as `clock` (none: they agree, or it heard none).
    fn clock(&mut self, clock: Option<ClockRead>) {
        let off = clock
            .as_ref()
            .is_some_and(|clock| matches!(clock.standing, accounts::ClockStanding::Off { .. }));
        self.table.writable = if off {
            Err(OFF_REFUSAL.to_owned())
        } else {
            Ok(())
        };
        self.table.clock = clock;
    }

    /// Whether this Mac's session on `account` keeps it at its next turn end: it stays, or reads
    /// its usage first; a move or a wait leaves it.
    fn keeps(&self, account: &str, now: i64) -> bool {
        let me = Me {
            account: Some(account),
            left: None,
            default_home: true,
            stored_login: true,
        };
        matches!(
            accounts::advise(
                &roster(),
                &self.state,
                &enrolled(),
                &me,
                &Point::TurnEnd,
                Mode::Early,
                now,
            ),
            Advice::Stay(_) | Advice::Probe
        )
    }

    /// [`Self::touch`] with `homes` (each account's key and its home Mac's label) for the holds
    /// this Mac keeps for the peers its table reads as skewed.
    fn touch_holding(&mut self, act: Act<'_>, homes: &[(LeaseKey, String)], now: i64) -> Touched {
        let presence = Presence {
            at: now,
            heard: self.heard.clone(),
        };
        touch_homes(
            &self.table,
            Some(presence),
            &mut self.state,
            Cli::Claude,
            &keys(Cli::Claude, &roster()),
            homes,
            act,
            now,
        )
    }
}

/// A's peers read its clock as off, and its own view still shows B's senior lease on A's home
/// row: A's home row is Ok all the same, any other row is `ClockOff`, its session on one moves or
/// waits at its turn end, and its claims write nothing.
///
/// Negative controls: with an Off Mac reading its home row by its own view (the `Off` arm of
/// `lease_verdict` for a home row left out), A's home row was refused (`left:
/// Err(LeasedElsewhere { .. })`, `right: Ok(())`); with `ClockOff` exempted from the turn-end
/// yield (`row_available_here` taking a `ClockOff` verdict as available), A's session kept its
/// lease (`its lease moves or waits at its turn end`).
#[test]
fn an_off_mac_keeps_its_home_row_behind_a_stale_lease() {
    let mut a = Mac::new("aa01", "m1");
    let mut b = Mac::new("bb02", "m2");
    a.state.home_accounts.insert("pool-a".into());
    b.runs("pool-a", NOW);
    assert_eq!(b.touch(Act::Claim("pool-a"), NOW), Touched::Claimed);
    replicate(&b, &a);
    a.hear(&b, NOW);
    a.clock(Some(clock_off()));
    a.runs("pool-b", NOW + 1);
    a.touch(Act::Look, NOW + 1);
    assert_eq!(
        a.state
            .leases
            .get("pool-a")
            .map(|lease| lease.holder.as_str()),
        Some("m2"),
        "A's view holds B's lease on its home row"
    );
    assert_eq!(
        a.state.clock.as_ref().map(|clock| clock.standing),
        Some(accounts::ClockStanding::Off { ahead: true })
    );
    assert_eq!(a.state.lease_verdict("pool-a", NOW + 2), Ok(()));
    assert_eq!(
        a.state.lease_verdict("pool-b", NOW + 2),
        Err(WhyNot::ClockOff)
    );
    assert!(
        !a.keeps("pool-b", NOW + 2),
        "its lease moves or waits at its turn end"
    );
    assert_eq!(
        a.touch(Act::Claim("pool-a"), NOW + 3),
        Touched::Unrecorded(OFF_REFUSAL.to_owned())
    );
    assert!(a.table.row("aa01").is_none(), "an Off Mac writes no row");
    // The record, renewed by each read, lapses with the reading behind it.
    assert!(a.state.clock_at(NOW + 2 + CLOCK_HOLD_S).is_some());
    assert_eq!(a.state.clock_at(NOW + 3 + CLOCK_HOLD_S), None);
}

/// A Mac its peers split over claims its home account and nothing else: a lease is refused and
/// written nowhere, and the home claim lands.
///
/// Negative control: without the Split arm in `touch_using` (`clock_refusal` refusing nothing
/// under Split), the lease was not refused (`a lease under Split`).
#[test]
fn a_tie_refuses_leases_and_keeps_home_claims() {
    let mut a = Mac::new("aa01", "m1");
    a.state.home_accounts.insert("pool-a".into());
    a.clock(Some(clock_split()));
    a.runs("pool-a", NOW);
    a.runs("pool-b", NOW);
    assert!(
        matches!(a.touch(Act::Claim("pool-b"), NOW), Touched::Unrecorded(_)),
        "a lease under Split"
    );
    assert!(a.table.row("aa01").is_none(), "a lease is written nowhere");
    assert_eq!(a.touch(Act::Claim("pool-a"), NOW + 1), Touched::Claimed);
    let row = a.table.row("aa01").expect("the home claim");
    assert_eq!(
        row.claims
            .iter()
            .map(|claim| (claim.key.clone(), claim.kind))
            .collect::<Vec<_>>(),
        vec![(key("pool-a"), Kind::Home)]
    );
    assert_eq!(
        a.state.lease_verdict("pool-b", NOW + 2),
        Err(WhyNot::ClockOff)
    );
    assert_eq!(a.state.lease_verdict("pool-a", NOW + 2), Ok(()));
}

/// B reads A, pool-a's home Mac, as skewed: B's lease on pool-a yields at its turn end, B's claim
/// there is held elsewhere, and C never cold-claims it, until the reading is no longer fresh.
///
/// Negative control: with no hold (`holds` returning none), B's claim on pool-a stood (`B's
/// claim on A's home row: Held`).
#[test]
fn a_skewed_peers_home_rows_are_held_for_it() {
    let homes = [(key("pool-a"), "m1".to_owned())];
    let mut b = Mac::new("bb02", "m2");
    b.runs("pool-a", NOW);
    assert_eq!(
        b.touch_holding(Act::Claim("pool-a"), &homes, NOW),
        Touched::Claimed
    );
    assert!(b.keeps("pool-a", NOW));
    let until = NOW + 30 * MIN;
    b.table.skewed = vec![skewed_peer("aa01", "m1", until)];
    match b.touch_holding(Act::Claim("pool-a"), &homes, NOW + 1) {
        Touched::HeldElsewhere(holder) => {
            assert_eq!(
                (holder.machine.as_str(), holder.label.as_str(), holder.kind),
                ("aa01", "m1", Kind::Home)
            );
            assert_eq!(holder.until, until);
        }
        other => panic!("B's claim on A's home row: {other:?}"),
    }
    assert!(!b.keeps("pool-a", NOW + 1), "B's lease yields");
    let mut c = Mac::new("cc03", "m3");
    c.table.skewed = vec![skewed_peer("aa01", "m1", until)];
    assert!(matches!(
        c.touch_holding(Act::Claim("pool-a"), &homes, NOW + 2),
        Touched::HeldElsewhere(_)
    ));
    assert_eq!(
        c.touch_holding(Act::Claim("pool-a"), &homes, until),
        Touched::Claimed,
        "the hold ends with the reading"
    );
}

/// B reads A, pool-a's home Mac, within the tolerance yet past the hold line, as a peer that
/// counted A Off does while A sits between half the tolerance and the tolerance: B names nobody
/// skewed, and still its claim on pool-a is held elsewhere and its lease there yields.
///
/// Negative control: with the holds taken from the peers the table names skewed (`holds(&read.
/// skewed, ..)`), B's claim stood (`B's claim on A's home row: Held`).
#[test]
fn a_peer_held_but_not_named_keeps_its_home_rows() {
    let homes = [(key("pool-a"), "m1".to_owned())];
    let mut b = Mac::new("bb02", "m2");
    b.runs("pool-a", NOW);
    assert_eq!(
        b.touch_holding(Act::Claim("pool-a"), &homes, NOW),
        Touched::Claimed
    );
    b.table.held = vec![skewed_peer("aa01", "m1", NOW + 30 * MIN)];
    match b.touch_holding(Act::Claim("pool-a"), &homes, NOW + 1) {
        Touched::HeldElsewhere(holder) => assert_eq!(holder.machine, "aa01"),
        other => panic!("B's claim on A's home row: {other:?}"),
    }
    assert!(!b.keeps("pool-a", NOW + 1), "B's lease yields");
}

/// A held peer's lease stays live past its row's `until` by as far as this Mac's clock runs ahead
/// of the peer's, while the reading of it is fresh and its Mac is heard: a peer two days behind
/// keeps it two days more, one 10 minutes behind keeps it 10 minutes more and no longer, and one
/// whose clock runs ahead keeps none. It lapses once the reading is stale, or once its Mac goes
/// unheard.
///
/// Negative controls: with `View::live` ignoring the held peers, B claimed pool-b past A's row's
/// `until` (`a lease two days behind, past its row`); with every held peer's claims live until the
/// reading lapses (the offset ignored), B could not claim pool-b once the 10 minutes passed
/// (`left: HeldElsewhere(..)`, `right: Claimed`).
#[test]
fn a_held_peers_visible_claims_outlast_until_by_its_offset_while_heard() {
    let mut a = Mac::new("aa01", "m1");
    a.runs("pool-b", NOW);
    assert_eq!(a.touch(Act::Claim("pool-b"), NOW), Touched::Claimed);
    let until = a.table.row("aa01").expect("A's row").until;
    let observer = |label: &str, reading_until: i64, ahead_s: i64, heard: i64, at: i64| {
        let mut b = Mac::new(&format!("{label}{label}"), label);
        replicate(&a, &b);
        b.heard.insert("aa01".to_owned(), heard);
        b.table.held = vec![SkewedPeer {
            ahead_s,
            ..skewed_peer("aa01", "m1", reading_until)
        }];
        b.runs("pool-b", at);
        b
    };
    let past = until + MIN;
    let mut days = observer("b2", past + 10 * MIN, 2 * 24 * HOUR, past, past);
    assert!(
        matches!(
            days.touch(Act::Claim("pool-b"), past),
            Touched::HeldElsewhere(_)
        ),
        "a lease two days behind, past its row"
    );
    let mut minutes = observer("b3", past + 30 * MIN, 10 * MIN, past, past);
    assert!(matches!(
        minutes.touch(Act::Claim("pool-b"), past),
        Touched::HeldElsewhere(_)
    ));
    let later = until + 10 * MIN;
    let mut minutes = observer("b4", later + 20 * MIN, 10 * MIN, later, later);
    assert_eq!(minutes.touch(Act::Claim("pool-b"), later), Touched::Claimed);
    let mut ahead = observer("b5", past + 10 * MIN, -2 * 24 * HOUR, past, past);
    assert_eq!(ahead.touch(Act::Claim("pool-b"), past), Touched::Claimed);
    let mut stale = observer("b6", past, 2 * 24 * HOUR, past, past);
    assert_eq!(stale.touch(Act::Claim("pool-b"), past), Touched::Claimed);
    let mut unheard = observer(
        "b7",
        past + 10 * MIN,
        2 * 24 * HOUR,
        past - PRESENCE_TTL_S - 1,
        past,
    );
    assert_eq!(unheard.touch(Act::Claim("pool-b"), past), Touched::Claimed);
}

/// A hold keys the account as its claims do: an `ai-pool.toml` account whose roster row expects
/// another identity than its login address (an alias login) is held under the roster's key,
/// which every claim and `Book::senior` use, and under its login's key too, so a peer whose
/// roster still names it by login is held off as well.
///
/// Negative control: with `home_keys` keying each account by its pool login alone, B claimed
/// pool-a under the roster's key while A was held (`B's claim on pool-a under the roster's key:
/// Claimed`).
#[test]
fn a_hold_keys_the_account_as_its_claims_do() {
    let login = LeaseKey::new("claude", "alias-login@example.test").expect("a key");
    let pool = [("pool-a".to_owned(), login.clone(), "m1".to_owned())];
    let homes = home_keys(&pool, &keys(Cli::Claude, &roster()));
    let mut b = Mac::new("bb02", "m2");
    b.table.held = vec![skewed_peer("aa01", "m1", NOW + 30 * MIN)];
    b.runs("pool-a", NOW);
    match b.touch_holding(Act::Claim("pool-a"), &homes, NOW) {
        Touched::HeldElsewhere(holder) => assert_eq!(holder.machine, "aa01"),
        other => panic!("B's claim on pool-a under the roster's key: {other:?}"),
    }
    assert_eq!(
        homes,
        vec![(key("pool-a"), "m1".to_owned()), (login, "m1".to_owned())]
    );
    // An account the roster keys by its login is held once.
    let same = [("pool-b".to_owned(), key("pool-b"), "m1".to_owned())];
    assert_eq!(home_keys(&same, &keys(Cli::Claude, &roster())).len(), 1);
}

/// A liar that makes itself look skewed gets holds on its own home accounts and no other: an
/// account whose home is another Mac stays free, an alias names it as its primary label does, and
/// a label that only starts like its own never does.
///
/// Negative control: with `holds` holding every home account for the first skewed peer (whatever
/// Mac the home names), pool-b was held too (`left: [("pool-a@example.test", "aa01"),
/// ("pool-b@example.test", "aa01")]`). With no hold at all, pool-a was not held (`left: []`).
#[test]
fn a_liar_that_looks_skewed_holds_only_its_own_home_rows() {
    let homes = [
        (key("pool-a"), "m1".to_owned()),
        (key("pool-b"), "m3".to_owned()),
    ];
    let liar = vec![skewed_peer("aa01", "m1", NOW + 30 * MIN)];
    let held = |skewed: &[SkewedPeer], homes: &[(LeaseKey, String)]| {
        holds(skewed, homes)
            .into_iter()
            .map(|(key, holder)| (key.email, holder.machine))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        held(&liar, &homes),
        vec![("pool-a@example.test".to_owned(), "aa01".to_owned())]
    );
    let mut b = Mac::new("bb02", "m2");
    b.table.skewed = liar.clone();
    assert!(matches!(
        b.touch_holding(Act::Claim("pool-a"), &homes, NOW),
        Touched::HeldElsewhere(_)
    ));
    b.runs("pool-b", NOW);
    assert_eq!(
        b.touch_holding(Act::Claim("pool-b"), &homes, NOW),
        Touched::Claimed
    );
    let aliased = vec![SkewedPeer {
        labels: vec!["m1".to_owned(), "m7".to_owned()],
        ..liar[0].clone()
    }];
    assert_eq!(held(&aliased, &[(key("pool-b"), "M7".to_owned())]).len(), 1);
    assert!(held(&liar, &[(key("pool-b"), "m10".to_owned())]).is_empty());
}

/// The clock record survives the store (`state.json`'s `claims_clock`): each standing, its counts
/// and its peers read back, and a record past its `until` is not kept.
///
/// Negative control: with `from_json` reading `ahead` as false whatever the file says, the record
/// read back behind (`left: Some(Clock { standing: Off { ahead: false }, .. })`).
#[test]
fn the_clock_record_survives_the_store_and_lapses_there() {
    for standing in [
        accounts::ClockStanding::Off { ahead: true },
        accounts::ClockStanding::Off { ahead: false },
        accounts::ClockStanding::Split,
    ] {
        let state = accounts::State {
            clock: Some(accounts::Clock {
                standing,
                by_s: HOUR,
                against: 2,
                of: 3,
                peers: vec!["m3".to_owned()],
                until: NOW + 10,
            }),
            ..accounts::State::default()
        };
        let text = state.to_json();
        let read = |now| {
            accounts::State::from_json(&text, now)
                .expect("a state")
                .clock
        };
        assert_eq!(read(NOW), state.clock, "{standing:?}");
        assert_eq!(read(NOW + 10), None, "{standing:?}");
    }
}

/// The code's step for each `AccountClaimClock` action: A's standing, its stale view, its
/// session and B's hold are set as the model's state says; `Verdict` and `Launch` are
/// `lease_verdict` on A's home row and on a row that is not, `TurnEnd` the turn-end advice of A's
/// session on that row, `BRound` B's look with A read as skewed (the holds, then its turn-end
/// advice), and `BCold` B's claim on A's home row. `Judge`, `StalePeer` and `Skew` (the
/// environment) step as the model does.
pub(crate) fn claim_clock_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    let homes = [(key("pool-a"), "m1".to_owned())];
    let mut a = Mac::new("aa01", "m1");
    a.state.home_accounts.insert("pool-a".into());
    a.state.clock = match state["st"] {
        2 | 3 => Some(accounts::Clock {
            standing: if state["st"] == 3 {
                accounts::ClockStanding::Off { ahead: true }
            } else {
                accounts::ClockStanding::Split
            },
            by_s: 0,
            against: 1,
            of: 2,
            peers: Vec::new(),
            until: NOW + CLOCK_HOLD_S,
        }),
        _ => None,
    };
    if state["plive"] == 1 {
        a.state.note_lease("pool-a", "m2", NOW + HOUR, NOW);
    }
    if state["lease"] == 1 {
        a.runs("pool-b", NOW);
    }
    // B, holding a claim on pool-a where `bcold` says so, and reading A as skewed where `skewed`.
    let peer_b = |holds: bool, skewed: bool| {
        let mut b = Mac::new("bb02", "m2");
        if holds {
            b.runs("pool-a", NOW);
            assert_eq!(
                b.touch_holding(Act::Claim("pool-a"), &homes, NOW),
                Touched::Claimed
            );
        }
        if skewed {
            b.table.skewed = vec![skewed_peer("aa01", "m1", NOW + 30 * MIN)];
        }
        b
    };
    match action {
        "Judge" => {
            next.insert("st", (state["st"] + 1) % 4);
            next.insert("turned", 0);
            next.insert("homeok", 2);
            // The round that moved A Off read it past the hold line on B's side too (aterm-db's
            // `every_peer_that_counted_an_off_mac_holds_for_it`).
            if state["st"] == 2 {
                next.insert("askew", 1);
            }
        }
        "StalePeer" => {
            next.insert("plive", 1);
            next.insert("homeok", 2);
        }
        "Skew" => {
            next.insert("askew", 1 - state["askew"]);
        }
        "Launch" => {
            let launches = a.state.lease_verdict("pool-b", NOW + 1).is_ok();
            next.insert("lease", i64::from(launches));
        }
        "Verdict" => {
            let ok = a.state.lease_verdict("pool-a", NOW + 1).is_ok();
            next.insert("homeok", i64::from(ok));
        }
        "TurnEnd" => {
            if state["st"] >= 2 {
                next.insert("turned", 1);
            }
            if state["lease"] == 1 && !a.keeps("pool-b", NOW + 1) {
                next.insert("lease", 0);
            }
        }
        "BRound" => {
            let skewed = state["askew"] == 1;
            let mut b = peer_b(state["bcold"] == 1, skewed);
            b.touch_holding(Act::Look, &homes, NOW + 1);
            next.insert("bseen", state["askew"]);
            if state["bcold"] == 1 && !b.keeps("pool-a", NOW + 1) {
                next.insert("bcold", 0);
            }
        }
        "BCold" => {
            // A claim reads the table: B's latest reading of A, held or not.
            let mut b = peer_b(false, state["askew"] == 1);
            b.runs("pool-a", NOW + 1);
            let claimed =
                b.touch_holding(Act::Claim("pool-a"), &homes, NOW + 1) == Touched::Claimed;
            next.insert("bcold", i64::from(claimed));
            next.insert("bseen", state["askew"]);
            if claimed && state["st"] == 3 {
                next.insert("bnew", 1);
            }
        }
        other => panic!("no AccountClaimClock step {other}"),
    }
    next
}

/// Every enabled `AccountClaimClock` step of the code is a model successor; A launches onto a row
/// that is not its home exactly where the model enables `Launch`, and B cold-claims A's home row
/// exactly where it enables `BCold`.
///
/// Negative controls: the model's own, an Off Mac whose home verdict is refused breaks
/// `OffKeepsItsHome` and is no `Verdict` successor, and a Split Mac going Off with B not holding
/// it is no `Judge` successor, B's cold claim then breaking `NoColdOnAnOffHome`. In the code:
/// - with the `Off` arm of `lease_verdict` for a home row left out, `Verdict` from an Off state
///   with a stale peer lease gave homeok 0, no model successor (`Verdict from {"askew": 1,
///   "bcold": 0, "bnew": 0, "bseen": 0, "homeok": 2, "lease": 0, "plive": 1, "st": 3, "turned":
///   0} gave {.., "homeok": 0, ..}`);
/// - with `ClockOff` exempted from the turn-end yield (`row_available_here` taking a `ClockOff`
///   verdict as available), a Split Mac kept its lease (`TurnEnd from {"askew": 0, "bcold": 0,
///   "bnew": 0, "bseen": 0, "homeok": 2, "lease": 1, "plive": 0, "st": 2, "turned": 0} gave {..,
///   "lease": 1, .., "turned": 1}`);
/// - with no hold (`holds` finding no peer), B cold-claimed A's home row while it held A (`BCold
///   from {"askew": 1, "bcold": 0, "bnew": 0, "bseen": 0, "homeok": 2, "lease": 0, "plive": 0,
///   "st": 0, "turned": 0}`, `left: true`, `right: false`).
#[test]
fn account_claim_clock_conforms() {
    let model = aterm_models_harness::account_claim_clock_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        if state["lease"] == 0 {
            let launched = claim_clock_transition(&state, "Launch")["lease"] == 1;
            assert_eq!(
                launched,
                model.action_enabled("Launch", &state),
                "Launch from {state:?}"
            );
        }
        if state["bcold"] == 0 {
            let cold = claim_clock_transition(&state, "BCold")["bcold"] == 1;
            assert_eq!(
                cold,
                model.action_enabled("BCold", &state),
                "BCold from {state:?}"
            );
        }
        for action in &model.actions {
            if !model.action_enabled(action.name, &state) {
                continue;
            }
            let next = claim_clock_transition(&state, action.name);
            assert!(
                model.successors(action.name, &state).contains(&next),
                "{} from {state:?} gave {next:?}",
                action.name
            );
        }
    }
    let off = aterm_spec::interp::State::from([
        ("st", 3),
        ("plive", 1),
        ("lease", 0),
        ("turned", 0),
        ("homeok", 2),
        ("askew", 1),
        ("bseen", 0),
        ("bcold", 0),
        ("bnew", 0),
    ]);
    let mut refused = off.clone();
    refused.insert("homeok", 0);
    assert!(!model.successors("Verdict", &off).contains(&refused));
    assert!(!model.check_invariant("OffKeepsItsHome", &refused));
    // A Split Mac going Off: B, whose reading counted it, holds it at once (the band case, where B
    // judging against its own tolerance reads A as agreeing, is what this rules out), so no cold
    // claim of B's on A's home row follows while A is Off.
    let split = aterm_spec::interp::State::from([
        ("st", 2),
        ("plive", 0),
        ("lease", 0),
        ("turned", 0),
        ("homeok", 1),
        ("askew", 0),
        ("bseen", 0),
        ("bcold", 0),
        ("bnew", 0),
    ]);
    let mut unheld = split.clone();
    unheld.insert("st", 3);
    unheld.insert("homeok", 2);
    assert!(!model.successors("Judge", &split).contains(&unheld));
    let mut cold = unheld.clone();
    cold.insert("bcold", 1);
    cold.insert("bnew", 1);
    assert!(!model.check_invariant("NoColdOnAnOffHome", &cold));
    assert_eq!(claim_clock_transition(&split, "Judge")["askew"], 1);
    assert_eq!(
        claim_clock_transition(&unheld, "BCold")["bcold"],
        1,
        "an unheld B claims A's home row"
    );
    let mut held = unheld;
    held.insert("askew", 1);
    assert_eq!(claim_clock_transition(&held, "BCold")["bcold"], 0);
}
