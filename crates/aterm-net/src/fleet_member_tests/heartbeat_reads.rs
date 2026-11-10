// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a heartbeat round reads of the state directory: the cached surveys of a swarm are
//! judged by their heads, and the round's own survey is the one record it reads whole.

use super::*;
use aterm_uds::private_file::tally;

/// Each cached survey of a 64-node swarm, about the 13 KB a live node's cache held a record.
const SURVEY_BYTES: usize = 13_000;
const SWARM: u8 = 64;

/// A round with two members answering, over a book that also binds the rest of a 64-node swarm
/// and a cache holding each node's survey, reads the cached surveys' heads only, and the round's
/// push reads this Mac's own survey alone. Reading each record whole (the prune's way before)
/// reads the cache once a prune, five times over in such a round. The heads still judge: each
/// record's is read by both of the round's prunes, and a record cached under a key the log took
/// out is removed by the first.
#[test]
fn a_heartbeat_round_reads_the_cached_surveys_heads_and_its_own_survey_once() {
    let secret_bytes = [0x5a; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let (a_state, b_state, c_state) = (
        State::new("hb-reads-a"),
        State::new("hb-reads-b"),
        State::new("hb-reads-c"),
    );
    for state in [&a_state, &b_state, &c_state] {
        write_key(state.dir(), &secret_bytes).unwrap();
    }
    let a = identity(&node_of(0x01), 0x01);
    let b = identity(&node_of(0x02), 0x02);
    let c = identity(&node_of(0x03), 0x03);
    keep_identity(b_state.dir(), &b, 0x02);
    let dark: Vec<NodeIdentity> = (0x10..0x10 + SWARM - 3)
        .map(|seed| identity(&node_of(seed), seed))
        .collect();
    // Out of the log and out of the book: only its cached record's head names its key.
    let gone = identity(&node_of(0x0f), 0x0f);
    let log: MemberKeys = [&a, &b, &c]
        .into_iter()
        .chain(&dark)
        .map(|who| (who.key(), Standing::Member))
        .chain([(gone.key(), Standing::Out("removed".to_owned()))])
        .collect();
    let a_at = Issuing::member(secret.clone(), a_state.dir(), a.clone(), log.clone());
    let c_at = Issuing::member(secret.clone(), c_state.dir(), c.clone(), log.clone());
    // A port nothing listens on: the rest of the swarm refuses its dial at once.
    let closed = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let entry = |who: &NodeIdentity, port: u16, via: Via| Peer {
        node: Some(who.node().to_owned()),
        host: "127.0.0.1".to_owned(),
        via,
        seen_ms: unix_ms_now(),
        ..keyed(0, who.key(), [127, 0, 0, 1], port)
    };
    let book: Vec<Peer> = [
        entry(&a, a_at.port, Via::Issuer),
        entry(&c, c_at.port, Via::Joined),
    ]
    .into_iter()
    .chain(dark.iter().map(|who| entry(who, closed, Via::Joined)))
    .collect();
    write_book(b_state.dir(), &book);
    let survey = format!("{}\n", "f".repeat(SURVEY_BYTES));
    for who in [&a, &b, &c, &gone].into_iter().chain(&dark) {
        store_pushed_facts_with_key(b_state.dir(), who.node(), Some(who.key()), &survey).unwrap();
    }
    let cache: usize = std::fs::read_dir(b_state.dir().join(SWARM_FACTS_DIR))
        .unwrap()
        .map(|entry| usize::try_from(entry.unwrap().metadata().unwrap().len()).unwrap())
        .sum();
    let reg = Registration {
        node: b.node().to_owned(),
        host: "m12.local".to_owned(),
        port: 7199,
        version: "0.146.0".to_owned(),
        key: None,
        signed: None,
    };
    let ((answered, own), read) = tally::counted(|| {
        let answered = heartbeat_members(
            b_state.dir(),
            (&secret, &b),
            &reg,
            HANG,
            (Some(&log), &Vec::new),
        )
        .expect("the book reads");
        // As `aterm-gui`'s round thread pushes its own survey to those that answered.
        let own = read_pushed_facts_of(b_state.dir(), b.node());
        let _ = Keyring::read(b_state.dir());
        (answered, own)
    });
    let mut nodes: Vec<&str> = answered.iter().map(|done| done.node.as_str()).collect();
    nodes.sort_unstable();
    assert_eq!(nodes, [a.node(), c.node()], "both live members answer");
    assert_eq!(
        own.as_deref(),
        Some(survey.as_str()),
        "the round's push reads its survey"
    );
    let surveys: usize = read
        .iter()
        .filter(|seen| seen.name.ends_with(".facts"))
        .map(|seen| seen.bytes)
        .sum();
    let total: usize = read.iter().map(|seen| seen.bytes).sum();
    eprintln!("cache {cache} B; a round read {surveys} B of it and {total} B in all: {read:?}");
    let head = PUSHED_FACTS_MAGIC.len() + 65;
    let record = PUSHED_FACTS_MAGIC.len() + 65 + survey.len();
    let gone_record = format!("{}.facts", gone.node());
    let read_of = |name: &str| read.iter().find(|seen| seen.name == name);
    assert!(
        !b_state
            .dir()
            .join(SWARM_FACTS_DIR)
            .join(&gone_record)
            .exists(),
        "the record under a key the log took out is pruned by its head"
    );
    let gone_read = read_of(&gone_record).expect("the first prune judges the held-out record");
    assert_eq!(
        (gone_read.reads, gone_read.bytes),
        (1, head),
        "the held-out record is judged by its head alone, then removed: {gone_read:?}"
    );
    for who in [&a, &b, &c].into_iter().chain(&dark) {
        let name = format!("{}.facts", who.node());
        let seen = read_of(&name).unwrap_or_else(|| panic!("no prune judged {name}"));
        // Both prunes judge every record by its head; the push reads this Mac's own whole.
        let expected = if who.node() == b.node() {
            (3, 2 * head + record)
        } else {
            (2, 2 * head)
        };
        assert_eq!(
            (seen.reads, seen.bytes),
            expected,
            "a round judges each cached survey by its head once before its dials and once after, \
             and reads only its own past the head: {seen:?}"
        );
    }
}
