// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The node service's reader of its peers' lifts ([`Reader`]): the time its lift is recorded at.

use super::{Peers, Reader};
use aterm_agent::harness::accounts;
use aterm_metrics::row::{Label, Lift, QuotaRow, Word};
use aterm_metrics::subject::{AccountKey, Cli, Node};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::io;

const T0: i64 = 1_790_000_000;
const RESET: i64 = T0 + 2 * 3600;
const POOL_A: &str = "ac-00000000000000a1";

/// A peer's quota row of `pool-a` lifting its five-hour wall by a proof at `T0 + 5`, as the log
/// holds it; while it is read, a session of this Mac meets the same wall again at `T0 + 3`.
struct Meanwhile<'a> {
    store: &'a accounts::Store,
    peer: Node,
}

impl Peers for Meanwhile<'_> {
    fn rows(&self) -> io::Result<BTreeMap<(Node, Cli, AccountKey), QuotaRow>> {
        let events = self
            .store
            .transact(T0 + 3, &mut None, |state| {
                Ok(accounts::note_wall(
                    state,
                    "pool-a",
                    accounts::SESSION_LIMIT,
                    Some(RESET),
                    T0 + 3,
                    T0 + 3,
                ))
            })
            .map_err(io::Error::other)?;
        assert_eq!(events, [], "the wall seen again is no new wall");
        let account = AccountKey::parse(POOL_A).expect("a key");
        let at = |t: i64| u64::try_from(t).expect("a time");
        let row = QuotaRow {
            at: at(T0 + 5),
            cli: Cli::Claude,
            account,
            label: Label::new("pool-a").expect("a label"),
            condition: Word::new("usable").expect("a word"),
            enrolled: true,
            pct5: Some(20),
            pct7: None,
            five_hour_reset: Some(at(RESET)),
            week_reset: None,
            walled_until: None,
            last_used: None,
            signin: None,
            last_start: None,
            last_end: None,
            sessions_7d: 0,
            plan: None,
            lifts: vec![Lift {
                window: Word::new(accounts::SESSION_LIMIT).expect("a word"),
                at: at(T0 + 5),
                reset: Some(at(RESET)),
            }],
        };
        Ok(BTreeMap::from([((self.peer, Cli::Claude, account), row)]))
    }
}

/// A PEER'S LIFT IS RECORDED AT THE TIME OF ITS TRANSACTION (FL-117): the reader reads the log
/// (up to its bound) and waits for the store's lock after it first reads the clock, and a
/// session that meets the standing wall again meanwhile writes its wall row at a later time. The
/// lift's mark reads the clock inside its transaction, so it is later than that row: the wall the
/// session waits at reads as lifted after it ([`accounts::Account::cleared_since`]), as
/// `Advice::Resume` asks. The sighting came before the peer's proof, so the lift still takes.
/// NEGATIVE CONTROL: a clock read once before the log, as the mark's time, leaves that session's
/// wall row newer than the mark, and the session would not resume.
#[test]
fn a_session_that_met_the_wall_while_the_peers_were_read_resumes_at_their_lift() {
    let mark = |clock: &dyn Fn() -> i64| {
        let dir = aterm_tempfile::TempDir::new().expect("a scratch dir");
        let harness = dir.path().join("harness");
        let store = accounts::Store::at(&harness);
        let mut state = accounts::State {
            machine: "0a0b".to_owned(),
            ..accounts::State::default()
        };
        let walled = accounts::note_wall(
            &mut state,
            "pool-a",
            accounts::SESSION_LIMIT,
            Some(RESET),
            T0 - 100,
            T0 - 100,
        );
        assert_eq!(walled.len(), 1, "{walled:?}");
        state.accounts.entry("pool-a".to_owned()).or_default().pk = Some(POOL_A.to_owned());
        store.save(&state).expect("the store saves");
        let peers = Meanwhile {
            store: &store,
            peer: Node::of_key(&[8; 32]),
        };
        let reader = Reader::new(Some(harness.clone()), None);
        let read = reader.read(&peers, Node::of_key(&[7; 32]), clock);
        assert_eq!((read.looked, read.lifted, read.failed), (true, 1, None));
        let (state, _) = store.load(T0 + 6).expect("the store reads");
        let account = state.accounts["pool-a"].clone();
        let wall = &account.walls[accounts::SESSION_LIMIT];
        assert!(wall.cleared, "{wall:?}");
        assert_eq!(wall.seen, Some(T0 + 5), "the peer's proof");
        (
            wall.at,
            account.cleared_since(accounts::SESSION_LIMIT, T0 + 3, T0 + 6),
        )
    };
    let calls = Cell::new(0);
    let moving = || {
        calls.set(calls.get() + 1);
        if calls.get() == 1 { T0 } else { T0 + 6 }
    };
    assert_eq!(
        mark(&moving),
        (T0 + 6, Some(T0 + 6)),
        "the mark is the transaction's time, and the session's wall is older"
    );

    // NEGATIVE CONTROL.
    assert_eq!(
        mark(&|| T0),
        (T0, None),
        "a mark read before the log is older than the session's wall"
    );
}
