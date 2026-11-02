// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The untaken-heads memo on its own: when it applies and how much it keeps.

use super::*;
use crate::membership_log::{Body, Entrant, Log, Record, with_kind_code};

/// A kind code no build writes yet: a test's stand-in for a newer build's record.
const NEWER_KIND: u8 = 0xee;

/// A founder's genesis and a store holding it and one `Name` it signed, with the bytes of the
/// founder's next record rewritten to a kind this build does not know.
fn rooted_with_one_unread() -> (Store, Vec<u8>) {
    let writer = [1u8; 32];
    let incarnation = [1u8; 16];
    let request = crate::commons::binding::Request::create(
        [0x31; 16],
        [0x42; 32],
        [0x21; 16],
        incarnation,
        &crate::age::Identity::from_secret_bytes([0x41; 32]),
        &writer,
    )
    .expect("request");
    let founder = request.binding().actor();
    let entrant = Entrant {
        request,
        node_key: [0x81; 32],
        name: "mac-1".to_owned(),
        label: "m1".to_owned(),
    };
    let genesis = Record::found(&[7; 32], vec![entrant], 1_000).expect("genesis");
    let mut log = Log::new(genesis.clone()).expect("log");
    let name = |log: &Log, n: u64| {
        log.draft(
            &founder,
            2_000 + n,
            Body::Name {
                name: format!("mac-one-{n}"),
            },
        )
        .sign(&writer, incarnation)
        .expect("signed")
    };
    let n0 = name(&log, 0);
    log.insert(n0.clone()).expect("held");
    let unread = with_kind_code(&name(&log, 1), NEWER_KIND);
    let mut store = Store::for_vault("team").expect("store");
    store.insert_many([genesis, n0]);
    assert!(store.log().is_some(), "the store holds its genesis");
    (store, unread)
}

/// One pass's notes against `store`, whose pulls named `have` from `named_from`.
fn noted<'a>(
    store: &'a Store,
    named_from: &'a [RecordId],
    now: Instant,
    source: Option<&'a str>,
) -> Noted<'a> {
    Noted {
        store,
        named_from,
        now,
        source,
    }
}

/// A memo names what the server sent that the store could not take for an hour of awake time
/// from when it was made, then the pull names the store's own ids again (a newer build may take
/// it by then); and a store with no genesis keeps no memo, since nothing before a genesis can be
/// checked and the store's complete part is empty.
/// NEGATIVE CONTROL: dropping the `since` check from `Gap::holds` names the head at
/// `t0 + UNTAKEN_AGAIN` ("the memo lapsed: left: 1, right: 0").
/// NEGATIVE CONTROL: dropping `note`'s guard for a store with no genesis (counting nothing held,
/// with an empty floor) names the head to the store with no genesis ("a store with no genesis
/// keeps no memo: left: 1, right: 0").
#[test]
fn a_memo_lapses_after_an_hour_and_never_applies_to_a_store_with_no_genesis() {
    let (store, unread) = rooted_with_one_unread();
    let head = aterm_digest::Sha256::digest(&unread);
    let sent = Sent::read(&unread);
    let memo: Untaken<u8> = Untaken::new();
    let url = "http://peer/v1/team";
    let t0 = Instant::now();
    let frontier = store.frontier();
    memo.note(0, url, &sent, &noted(&store, &frontier, t0, None));

    let named = memo.have(&0, url, &store, &store.have(), t0);
    assert_eq!(named.heads, 1, "the memo names the stream's head");
    assert_eq!(named.ids.first(), Some(&head));
    assert_eq!(&named.ids[1..], store.have().as_slice());
    let last_moment = t0 + UNTAKEN_AGAIN - Duration::from_millis(1);
    assert_eq!(
        memo.have(&0, url, &store, &store.have(), last_moment).heads,
        1
    );

    let lapsed = memo.have(&0, url, &store, &store.have(), t0 + UNTAKEN_AGAIN);
    assert_eq!(lapsed.heads, 0, "the memo lapsed");
    assert_eq!(lapsed.ids, store.have());
    assert_eq!(memo.kept(&0), 0, "a lapsed memo is dropped");

    let unrooted = Store::for_vault("team").expect("store");
    memo.note(1, url, &sent, &noted(&unrooted, &[], t0, None));
    let named = memo.have(&1, url, &unrooted, &unrooted.have(), t0);
    assert_eq!(named.heads, 0, "a store with no genesis keeps no memo");
    assert_eq!(named.ids, unrooted.have());
}

/// Past [`UNTAKEN_URLS`] servers a store's table drops the memo used longest ago, never the vault
/// source's: a LAN host that advertises a fresh URL on each pass, each sending one record the
/// store cannot take, pushes out its own memos, not those of the peers the passes keep asking,
/// nor the source's, which a pass that long-polls elsewhere may not ask for a while.
/// NEGATIVE CONTROL: dropping the memo made longest ago (`min_by_key` on `gap.since`) drops the
/// peers in use ("a memo in use stays: hot-0: left: 0, right: 1").
/// NEGATIVE CONTROL: dropping the memo used longest ago whoever's it is (`min_by_key` on
/// `gap.used` alone) drops the source's ("the vault source's memo stays: left: 0, right: 1").
#[test]
fn past_its_cap_the_table_drops_the_memo_used_longest_ago_never_the_sources() {
    let (store, unread) = rooted_with_one_unread();
    let sent = Sent::read(&unread);
    let frontier = store.frontier();
    let have = store.have();
    let memo: Untaken<u8> = Untaken::new();
    let t0 = Instant::now();
    let at = |i: usize| t0 + Duration::from_millis(u64::try_from(i).expect("small"));
    let source = "http://vault/v1/team";
    let pass = |i: usize| noted(&store, &frontier, at(i), Some(source));
    let hot = |i: usize| format!("http://hot-{i}/v1/team");
    let fresh = |i: usize| format!("http://fresh-{i}/v1/team");
    memo.note(0, source, &sent, &pass(0));
    for h in 0..4 {
        memo.note(0, &hot(h), &sent, &pass(0));
    }
    for i in 0..UNTAKEN_URLS + 8 {
        memo.note(0, &fresh(i), &sent, &pass(i + 1));
        if i % 16 == 0 {
            for h in 0..4 {
                memo.have(&0, &hot(h), &store, &have, at(i + 1));
            }
        }
    }
    assert_eq!(memo.kept(&0), UNTAKEN_URLS, "the table keeps its cap");
    let now = at(UNTAKEN_URLS + 9);
    assert_eq!(
        memo.have(&0, source, &store, &have, now).heads,
        1,
        "the vault source's memo stays"
    );
    for h in 0..4 {
        assert_eq!(
            memo.have(&0, &hot(h), &store, &have, now).heads,
            1,
            "a memo in use stays: hot-{h}"
        );
    }
    for i in 0..13 {
        assert_eq!(
            memo.have(&0, &fresh(i), &store, &have, now).heads,
            0,
            "the memo used longest ago went: fresh-{i}"
        );
    }
    assert_eq!(
        memo.have(&0, &fresh(UNTAKEN_URLS + 7), &store, &have, now)
            .heads,
        1,
        "the newest memo stays"
    );
}

/// A fleet the size of ALab's (86 servers), each holding a record the store cannot take and
/// asked in turn, 16 a pass beside the vault's source, keeps every server's memo until its next
/// turn, so no server sends the gap twice. Before, a table of 68 dropped each memo about four
/// passes after it was made, a pass or two before the rotation came back to that server, and
/// every peer sent the whole gap on every visit.
/// NEGATIVE CONTROL: the table's earlier cap (`UNTAKEN_URLS` of 68) sends a peer the gap again on
/// its second turn ("pass 5: http://peer-0/v1/team sent the gap again: left: 0, right: 1").
#[test]
fn a_fleet_asked_in_turn_keeps_each_servers_memo_until_its_next_turn() {
    let (store, unread) = rooted_with_one_unread();
    let sent = Sent::read(&unread);
    let frontier = store.frontier();
    let have = store.have();
    let memo: Untaken<u8> = Untaken::new();
    let source = "http://vault/v1/team";
    let peers: Vec<String> = (0..86)
        .map(|i| format!("http://peer-{i}/v1/team"))
        .collect();
    let per_pass = crate::discover::MAX_CANDIDATES;
    let t0 = Instant::now();
    let mut visited = BTreeSet::new();
    for pass in 0..3 * peers.len().div_ceil(per_pass) {
        let now = t0 + Duration::from_secs(u64::try_from(pass).expect("small"));
        let asked = std::iter::once(source)
            .chain((0..per_pass).map(|k| peers[(pass * per_pass + k) % peers.len()].as_str()));
        for url in asked {
            let named = memo.have(&0, url, &store, &have, now);
            if visited.contains(url) {
                assert_eq!(named.heads, 1, "pass {pass}: {url} sent the gap again");
            } else {
                assert_eq!(named.heads, 0, "pass {pass}: {url} was never asked");
                memo.note(0, url, &sent, &noted(&store, &frontier, now, Some(source)));
                visited.insert(url);
            }
        }
    }
    assert_eq!(visited.len(), peers.len() + 1, "every server was asked");
}
