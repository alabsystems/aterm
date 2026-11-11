// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::presence::{Build, Held};
use aterm_metrics::row::{AgentEntry, HeldEntry, ManagerEntry, NeedEntry};
use std::collections::BTreeSet;

const ME: &str = "n-00000000000000aa";
const PEER: &str = "n-00000000000000bb";

fn row(at_ms: u64) -> PresenceRow {
    PresenceRow {
        at_ms,
        build: Build::new("0.155.0+g0123abcd").expect("a build"),
        addresses: vec!["192.168.1.22:7480".parse().expect("an address")],
        membership: Some(Held::membership(&BTreeSet::from([[1; 32]]))),
        tables: None,
        topics: vec!["build".to_owned()],
    }
}

fn needs() -> NeedsRow {
    let sid = aterm_metrics::row::Sid::parse("s-0123456789abcdef0123").expect("a sid");
    NeedsRow {
        at: 1_793_836_800,
        needs: vec![NeedEntry {
            sid,
            word: aterm_metrics::row::Word::new("stalled").expect("a word"),
            harness: false,
            worker_of: None,
            since: Some(1_793_836_700),
        }],
        held: vec![HeldEntry {
            sid,
            hold: aterm_metrics::row::Word::new("person").expect("a word"),
            by: Node::parse(ME),
            since: None,
        }],
        managers: vec![ManagerEntry { sid, workers: 2 }],
    }
}

fn activity() -> ActivityRow {
    ActivityRow {
        at: 1_793_836_790,
        busy: 2,
        walled: 1,
        sessions: vec![AgentEntry {
            sid: aterm_metrics::row::Sid::parse("s-0123456789abcdef0123").expect("a sid"),
            word: aterm_metrics::row::Word::new("walled").expect("a word"),
            detail: aterm_metrics::row::Word::new("usage-weekly"),
        }],
        ..ActivityRow::default()
    }
}

fn view() -> View {
    View {
        me: ME.to_owned(),
        at_ms: 1_793_836_800_000,
        standings: true,
        seats: vec![
            Seat {
                principal: ME.to_owned(),
                key: [7; 32],
                me: true,
                standing: Standing::Up,
                behind: Behind::InStep,
                row: Some((41, row(1_793_836_790_000))),
            },
            Seat {
                principal: PEER.to_owned(),
                key: [8; 32],
                me: false,
                standing: Standing::Silent {
                    last_seen_ms: Some(1_793_836_000_000),
                },
                behind: Behind::Behind {
                    at_least: Duration::from_secs(300),
                },
                row: Some((9, row(1_793_836_000_000))),
            },
            Seat {
                principal: "n-00000000000000cc".to_owned(),
                key: [9; 32],
                me: false,
                standing: Standing::Silent { last_seen_ms: None },
                behind: Behind::Unknown,
                row: None,
            },
        ],
        needs: Some(BTreeMap::from([(PEER.to_owned(), needs())])),
        activity: Some(BTreeMap::from([(PEER.to_owned(), activity())])),
    }
}

/// A view reads back as itself, with or without its needs rows and its activity rows, and
/// nothing it does not write reads as one: a cut, a trailing byte, another version (version
/// 1's body, which ends at its needs), a principal that is no node id, a tag it never writes,
/// more seats than it holds, or an activity row named twice.
#[test]
fn a_view_reads_back_as_itself_and_nothing_else_reads_as_one() {
    let whole = view();
    assert_eq!(View::decode(&whole.encode()), Ok(whole.clone()));
    let unread = View {
        needs: None,
        ..whole.clone()
    };
    assert_eq!(View::decode(&unread.encode()), Ok(unread));
    let no_activity = View {
        activity: None,
        ..whole.clone()
    };
    assert_eq!(View::decode(&no_activity.encode()), Ok(no_activity.clone()));
    // Version 1 wrote no activity map: its body is this one less the last tag, under 1.
    let mut v1 = no_activity.encode();
    assert_eq!(v1.pop(), Some(0), "the activity map's tag ends it");
    v1[0] = 1;
    assert_eq!(View::decode(&v1), Err(ViewError::Version(1)));
    let twice = {
        let once = View {
            needs: None,
            ..whole.clone()
        }
        .encode();
        let entry = 1 + PEER.len() + 4 + activity().encode().len();
        let (head, row) = once.split_at(once.len() - entry);
        let mut twice = head.to_vec();
        let count_at = twice.len() - 4;
        twice[count_at..].copy_from_slice(&2u32.to_be_bytes());
        twice.extend_from_slice(row);
        twice.extend_from_slice(row);
        twice
    };
    assert_eq!(
        View::decode(&twice),
        Err(ViewError::Tag("an activity row named twice"))
    );
    let bytes = whole.encode();
    for cut in [0, 1, 10, bytes.len() / 2, bytes.len() - 1] {
        assert_eq!(
            View::decode(&bytes[..cut]),
            Err(ViewError::Truncated),
            "cut at {cut}"
        );
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(View::decode(&trailing), Err(ViewError::Trailing));
    let mut version = bytes.clone();
    version[0] = 3;
    assert_eq!(View::decode(&version), Err(ViewError::Version(3)));
    let mut principal = bytes.clone();
    principal[3] = b'X';
    assert_eq!(View::decode(&principal), Err(ViewError::Principal));
    // The standings flag sits after the version, the principal and the time.
    let flag_at = 1 + 1 + ME.len() + 8;
    let mut flag = bytes.clone();
    flag[flag_at] = 2;
    assert_eq!(View::decode(&flag), Err(ViewError::Tag("flag")));
    let mut many = bytes;
    many[flag_at + 1..flag_at + 5].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(View::decode(&many), Err(ViewError::TooMany));
}

/// The node answers a reader of its own uid one frame per ask, and ends its wait when
/// nudged; a stranger's bytes get no view, and the next reader is still answered.
#[test]
fn the_server_answers_each_ask_and_stops_when_nudged() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("av")
        .tempdir_in("/tmp")
        .expect("a scratch folder");
    let server = std::sync::Arc::new(
        Server::bind(dir.path())
            .expect("the socket binds")
            .expect("a short path"),
    );
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let serving = {
        let (server, stop) = (std::sync::Arc::clone(&server), std::sync::Arc::clone(&stop));
        std::thread::spawn(move || {
            server.run(&|| stop.load(std::sync::atomic::Ordering::Acquire), &view);
        })
    };
    let hang = Duration::from_secs(60);
    assert_eq!(ask(dir.path(), hang).expect("a view"), view());
    let mut stranger = UnixStream::connect(dir.path().join(SOCKET)).expect("a connection");
    stranger.set_read_timeout(Some(hang)).expect("a read bound");
    stranger.write_all(b"atm-view 1\n").expect("an ask written");
    let mut answered = Vec::new();
    stranger
        .read_to_end(&mut answered)
        .expect("the node hangs up");
    assert!(answered.is_empty(), "an older reader's ask gets no view");
    assert_eq!(ask(dir.path(), hang).expect("a view"), view());
    stop.store(true, std::sync::atomic::Ordering::Release);
    server.nudge();
    serving.join().expect("the server ends");
    drop(server);
    assert!(
        matches!(ask(dir.path(), hang), Err(Unserved::NoNode)),
        "a stopped node serves no view, and its socket is gone"
    );
}
