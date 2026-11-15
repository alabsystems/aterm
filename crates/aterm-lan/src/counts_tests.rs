// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_uds::peer::AuditToken;

use super::{Count, Counts, HEAD, Listed, REQUESTERS, Sink};

fn token(pid: u32, version: u32) -> AuditToken {
    AuditToken([0, 501, 20, 501, 20, pid, 0, version])
}

const fn listed(pid: u32, connections: u64, admitted: u64, since_accepted: u64) -> Listed {
    Listed {
        pid,
        connections,
        admitted,
        since_accepted,
    }
}

/// One more connection accepted, its check returned for the process `token` names.
fn connect(counts: &Counts, token: Option<AuditToken>, admitted: bool) {
    counts.add(Count::Accepted);
    counts.requester(token, admitted);
}

/// The law of the requesters' table. A requester is a whole token: the same pid under another
/// pid version is another requester, each connection is in exactly one requester's count with
/// whether its check admitted it, and a connection with no token is in none. A new requester
/// takes an empty slot, then the slot of the one seen longest ago that was never admitted, then
/// (admitted itself) the slot of the one seen longest ago: so a requester that keeps connecting
/// stays while [`REQUESTERS`] others come and go, one the check does not admit never replaces
/// one it admitted, and a requester that left and came back says a later start.
///
/// Negative controls: compare `known.token.pid() == token.pid()` in `Counts::requester` (the
/// two versions of pid 7 read as one requester: `[Listed { pid: 7, connections: 3, .. }]`);
/// delete `known.seen = seen` on a hit (the requester that kept connecting is replaced: pid 7
/// is not in the table); let a newcomer the check did not admit take an admitted requester's
/// slot (`.or_else(|| longest_ago(&table.slots, true))`: pid 901 is in the table).
#[test]
fn requesters_are_counted_by_whole_token_and_the_ones_seen_last_stay() {
    let counts = Counts::new();
    let new = || counts.get(Count::NewRequesters);
    connect(&counts, Some(token(7, 1)), true);
    connect(&counts, Some(token(7, 2)), true);
    connect(&counts, Some(token(7, 1)), false);
    connect(&counts, None, true);
    assert_eq!(
        counts.requesters(),
        [listed(7, 2, 1, 1), listed(7, 1, 1, 2)]
    );
    assert_eq!(new(), 2);
    // One the check refuses and five it admits fill the table.
    connect(&counts, Some(token(900, 1)), false);
    for pid in 100..105 {
        connect(&counts, Some(token(pid, 1)), true);
    }
    assert_eq!(counts.requesters().len(), REQUESTERS);
    assert_eq!(new(), 8);
    // A new one takes the slot of the one never admitted, though others were seen longer ago.
    connect(&counts, Some(token(200, 1)), true);
    let table = counts.requesters();
    assert_eq!(table[2], listed(200, 1, 1, 11), "{table:?}");
    assert_eq!(table[0].pid, 7, "{table:?}");
    // One the check refuses takes no admitted requester's slot, however often it connects.
    for _ in 0..2 {
        connect(&counts, Some(token(901, 1)), false);
    }
    assert_eq!(counts.requesters(), table);
    assert_eq!(new(), 11);
    // Eight more come and go, the first requester connecting between them: it stays, and the
    // seven others that were there are replaced, the one seen longest ago first.
    for pid in 300..308 {
        connect(&counts, Some(token(pid, 1)), true);
        connect(&counts, Some(token(7, 1)), true);
    }
    let table = counts.requesters();
    assert_eq!(table[0], listed(7, 10, 9, 1), "{table:?}");
    let mut others: Vec<u32> = table[1..].iter().map(|known| known.pid).collect();
    others.sort_unstable();
    assert_eq!(others, [301, 302, 303, 304, 305, 306, 307], "{table:?}");
    assert_eq!(new(), 19);
    // A requester the table dropped is new again, and says where its count starts.
    connect(&counts, Some(token(7, 2)), true);
    assert_eq!(new(), 20);
    let back = counts.requesters();
    let back = back
        .iter()
        .find(|known| known.pid == 7 && known.connections == 1);
    assert_eq!(back, Some(&listed(7, 1, 1, 30)));
}

/// Each name is printed once and each count has a name of its own.
#[test]
fn every_count_is_in_a_snapshot_under_its_own_name() {
    let counts = Counts::new();
    for (index, what) in Count::ALL.into_iter().enumerate() {
        assert_eq!(what as usize, index, "{what:?} is printed out of its slot");
        for _ in 0..=index {
            counts.add(what);
        }
    }
    let snapshot = counts.snapshot(42, 9);
    let line = snapshot.lines().next().unwrap();
    assert!(line.starts_with("at_ms=42 "), "{line}");
    assert!(line.ends_with(" process_cpu_ns=9"), "{line}");
    for (index, what) in Count::ALL.into_iter().enumerate() {
        let field = format!(" {}={} ", what.name(), index + 1);
        assert_eq!(line.matches(&field).count(), 1, "{field:?} in {line}");
    }
}

fn clock() -> u64 {
    static NOW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NOW.fetch_add(1000, std::sync::atomic::Ordering::Relaxed) + 1000
}

fn no_cpu() -> u64 {
    0
}

fn silent(_: std::fmt::Arguments<'_>) {}

/// The file holds the snapshot written last and the one written now, whole, and nothing else:
/// a third write drops the first.
#[test]
fn the_file_holds_the_previous_and_the_current_snapshot() {
    let root = aterm_tempfile::tempdir().unwrap();
    let socket = root.path().join("broker.sock");
    let file = root.path().join(aterm_uds::lan::COUNTS_NAME);
    let counts = Counts::new();
    let mut sink = Sink::beside_with(&socket, clock, no_cpu);
    let mut written = Vec::new();
    for _ in 0..3 {
        counts.add(Count::Accepted);
        sink.write(&counts, silent);
        written.push(std::fs::read_to_string(&file).unwrap());
    }
    let snapshot = |n: u64, at: u64| {
        let mut line = format!("at_ms={at} accepted={n}");
        for what in &Count::ALL[1..] {
            line.push_str(&format!(" {}=0", what.name()));
        }
        line + " checks_returned=0 check_cpu_ns=0 conn_cpu_ns=0 resolve_threads=0 \
                resolve_cpu_ns=0 process_cpu_ns=0\n"
    };
    let at = |text: &str| -> u64 {
        let line = text.lines().last().unwrap();
        line.split(' ').next().unwrap()["at_ms=".len()..]
            .parse()
            .unwrap()
    };
    let first = at(&written[0]);
    assert_eq!(
        written[0],
        format!("{HEAD}\nprevious\ncurrent\n{}", snapshot(1, first))
    );
    assert_eq!(
        written[2],
        format!(
            "{HEAD}\nprevious\n{}current\n{}",
            snapshot(2, first + 1000),
            snapshot(3, first + 2000)
        )
    );
    let left: Vec<_> = std::fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        left,
        [std::ffi::OsString::from(aterm_uds::lan::COUNTS_NAME)]
    );
}

fn logged(_: std::fmt::Arguments<'_>) {
    LOGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

static LOGGED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The sink writes into the folder that is there and makes none: with no folder for the file
/// nothing is written and nothing is made, the first such write is logged and no later one,
/// and once the folder is there the file is written, its previous snapshot the one that was
/// not.
///
/// Negative control: write without the look at the folder (`write_flushed` alone in
/// `Sink::write`): the folder is made (`assertion failed: !folder.exists()`).
#[test]
fn the_sink_writes_into_no_folder_it_would_have_to_make() {
    let root = aterm_tempfile::tempdir().unwrap();
    let folder = root.path().join("gone");
    let socket = folder.join("broker.sock");
    let counts = Counts::new();
    // A clock of its own: the scripted one above is another law's.
    let mut sink = Sink::beside_with(&socket, || 0, no_cpu);
    for _ in 0..2 {
        counts.add(Count::Accepted);
        sink.write(&counts, logged);
        assert!(!folder.exists());
    }
    assert_eq!(LOGGED.load(std::sync::atomic::Ordering::Relaxed), 1);
    std::fs::create_dir(&folder).unwrap();
    counts.add(Count::Accepted);
    sink.write(&counts, logged);
    let text = std::fs::read_to_string(folder.join(aterm_uds::lan::COUNTS_NAME)).unwrap();
    let accepted: Vec<&str> = text
        .split(' ')
        .filter(|field| field.starts_with("accepted="))
        .collect();
    assert_eq!(accepted, ["accepted=2", "accepted=3"]);
    assert_eq!(LOGGED.load(std::sync::atomic::Ordering::Relaxed), 1);
}
