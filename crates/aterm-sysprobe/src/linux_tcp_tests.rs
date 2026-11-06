// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;
use crate::jobs::TcpState;
use crate::jobs::linux::TcpRow;
use crate::linux::{Dump, DumpFault};

const SNMP: &str = "Ip: Forwarding DefaultTTL\nIp: 1 64\n\
Tcp: RtoAlgorithm RtoMin RtoMax MaxConn ActiveOpens PassiveOpens AttemptFails EstabResets CurrEstab InSegs OutSegs RetransSegs InErrs OutRsts InCsumErrors\n\
Tcp: 1 200 120000 -1 9101 433 76 120 7 4410922 4512893 3021 0 1450 0\n\
Udp: InDatagrams NoPorts\nUdp: 9 1\n";

const SOCKSTAT: &str =
    "sockets: used 1203\nTCP: inuse 12 orphan 0 tw 4 alloc 31 mem 2\nUDP: inuse 9 mem 4\n";

/// A netlink message of `kind` under `seq` with `flags`, padded to 4 bytes.
fn message(kind: u16, flags: u16, seq: u32, payload: &[u8]) -> Vec<u8> {
    let len = 16 + payload.len();
    let mut m = vec![0u8; len.next_multiple_of(4)];
    m[0..4].copy_from_slice(&u32::try_from(len).unwrap().to_ne_bytes());
    m[4..6].copy_from_slice(&kind.to_ne_bytes());
    m[6..8].copy_from_slice(&flags.to_ne_bytes());
    m[8..12].copy_from_slice(&seq.to_ne_bytes());
    m[16..len].copy_from_slice(payload);
    m
}

/// An `inet_diag_msg` of `family` in `state` (`net/tcp_states.h`), from local port `sport` to
/// `dst`:`dport`, with `inode`, `cookie` and `rqueue` unread bytes.
fn record(family: u8, state: u8, (sport, dst, dport): (u16, [u8; 16], u16), inode: u32) -> Vec<u8> {
    let mut r = vec![0u8; 72];
    r[0] = family;
    r[1] = state;
    r[4..6].copy_from_slice(&sport.to_be_bytes());
    r[6..8].copy_from_slice(&dport.to_be_bytes());
    r[24..40].copy_from_slice(&dst);
    r[44..52].copy_from_slice(&u64::from(inode).wrapping_mul(0x9e37).to_ne_bytes());
    r[56..60].copy_from_slice(&5u32.to_ne_bytes());
    r[68..72].copy_from_slice(&inode.to_ne_bytes());
    r
}

fn v4(a: [u8; 4]) -> [u8; 16] {
    let mut d = [0; 16];
    d[..4].copy_from_slice(&a);
    d
}

fn v6(lead: [u8; 6], last: u8) -> [u8; 16] {
    let mut d = [0; 16];
    d[..6].copy_from_slice(&lead);
    d[15] = last;
    d
}

fn done(seq: u32) -> Vec<u8> {
    message(3, 0, seq, &0i32.to_ne_bytes())
}

const SYN_SENT: u8 = 2;
const ESTABLISHED: u8 = 1;
const FIN_WAIT2: u8 = 5;
const TIME_WAIT: u8 = 6;
const CLOSE_WAIT: u8 = 8;
const LISTEN: u8 = 10;

/// TCP COUNTS COME FROM SNMP AND SOCKSTAT (A3 D1): a reading's `ESTABLISHED` is `CurrEstab` and its
/// `TIME_WAIT` is `sockstat`'s `tw`, read this reading; every other state is the newest dump's.
/// No connection counts twice: where `CurrEstab` counts `CLOSE_WAIT`, the dump's two come off it,
/// and the dump's timewait `FIN_WAIT2` socket comes off `tw`. NEGATIVE CONTROL: on a kernel whose
/// `CurrEstab` counts no `CLOSE_WAIT`, nothing comes off it.
#[test]
fn tcp_counts_come_from_snmp_and_sockstat() {
    assert_eq!(snmp_curr_estab(SNMP), Some(7));
    assert_eq!(sockstat_time_wait(SOCKSTAT), Some(4));
    assert_eq!(snmp_curr_estab("Tcp: CurrEstab\n"), None, "no values line");
    assert_eq!(sockstat_time_wait("TCP: inuse 3 orphan 0\n"), None);
    let mut dump = TcpDump {
        counts: TcpCounts {
            established: 40,
            time_wait: 90,
            close_wait: 2,
            fin_wait2: 3,
            listen: 9,
            syn_sent: 1,
            ..TcpCounts::default()
        },
        timewait_fin_wait2: 1,
        close_wait_in_curr_estab: true,
        ..TcpDump::default()
    };
    let split = TcpCounts {
        close_wait: 2,
        fin_wait2: 3,
        listen: 9,
        syn_sent: 1,
        ..TcpCounts::default()
    };
    assert_eq!(
        reading_counts(7, 4, Some(&dump)),
        TcpCounts {
            established: 5,
            time_wait: 3,
            ..split
        },
        "the counters' established and time-wait, the dump's other states"
    );
    dump.close_wait_in_curr_estab = false;
    assert_eq!(
        reading_counts(7, 4, Some(&dump)),
        TcpCounts {
            established: 7,
            time_wait: 3,
            ..split
        },
        "a CurrEstab with no CLOSE_WAIT in it"
    );
    assert_eq!(
        reading_counts(7, 4, None),
        TcpCounts {
            established: 7,
            time_wait: 4,
            ..TcpCounts::default()
        },
        "no dump yet: the counters alone, the rest unread"
    );
}

/// WHETHER `CurrEstab` COUNTS `CLOSE_WAIT` IS MEASURED, NOT ASSUMED: Linux 6.10 (a46d0ea5c949)
/// began counting it, so a dump compares `CurrEstab` beside it with its own `ESTABLISHED` and
/// `ESTABLISHED + CLOSE_WAIT`.
#[test]
fn whether_curr_estab_counts_close_wait_is_measured_beside_a_dump() {
    let dump = TcpCounts {
        established: 40,
        close_wait: 6,
        ..TcpCounts::default()
    };
    assert!(curr_estab_counts_close_wait(46, &dump), "6.10 and later");
    assert!(
        curr_estab_counts_close_wait(45, &dump),
        "a connection closed between"
    );
    assert!(!curr_estab_counts_close_wait(40, &dump), "before 6.10");
    assert!(!curr_estab_counts_close_wait(41, &dump));
    let none = TcpCounts {
        established: 40,
        ..TcpCounts::default()
    };
    assert!(
        curr_estab_counts_close_wait(40, &none),
        "no CLOSE_WAIT: either reading subtracts nothing"
    );
}

/// The all-states dump counts every record's state in both families, keeps each connection by
/// its inode (one in `TIME_WAIT`, which has none, is not kept), and finds the established ones to
/// a remote peer to follow; loopback is no remote peer. A fault in any datagram is the dump's.
#[test]
fn the_all_states_dump_counts_every_state_and_keeps_sockets_by_inode() {
    let mut first = message(
        20,
        0,
        9,
        &record(2, ESTABLISHED, (50_081, v4([192, 0, 2, 10]), 8080), 2222),
    );
    first.extend(message(
        20,
        0,
        9,
        &record(2, ESTABLISHED, (50_082, v4([127, 0, 0, 1]), 8080), 2223),
    ));
    first.extend(message(
        20,
        0,
        9,
        &record(2, LISTEN, (8080, [0; 16], 0), 1111),
    ));
    first.extend(message(
        20,
        0,
        9,
        &record(2, TIME_WAIT, (50_000, v4([192, 0, 2, 10]), 443), 0),
    ));
    let mut timewait = record(2, FIN_WAIT2, (50_001, v4([192, 0, 2, 10]), 443), 0);
    timewait[2] = 3;
    first.extend(message(20, 0, 9, &timewait));
    first.extend(message(
        20,
        0,
        9,
        &record(2, FIN_WAIT2, (50_002, v4([192, 0, 2, 10]), 443), 4444),
    ));
    let mut second = message(
        20,
        0,
        9,
        &record(
            10,
            CLOSE_WAIT,
            (40_000, v6([0x20, 1, 0x0d, 0xb8, 0, 0], 1), 443),
            3333,
        ),
    );
    second.extend(done(9));
    let mut read = DumpRead::default();
    assert_eq!(tcp_diag_records(&first, 9, &mut read), Ok(Dump::More));
    assert_eq!(tcp_diag_records(&second, 9, &mut read), Ok(Dump::Done));
    let dump = read.finish();
    assert_eq!(
        dump.counts,
        TcpCounts {
            established: 2,
            listen: 1,
            time_wait: 1,
            fin_wait2: 2,
            close_wait: 1,
            ..TcpCounts::default()
        }
    );
    assert_eq!(
        dump.timewait_fin_wait2, 1,
        "the FIN_WAIT2 socket held as a timewait one, not the full one"
    );
    assert_eq!(
        dump.sockets
            .keys()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        [1111, 2222, 2223, 3333, 4444].into(),
        "a timewait socket has no inode"
    );
    assert_eq!(
        dump.sockets[&2222],
        TcpRow {
            inode: 2222,
            state: TcpState::Established,
            local_port: 50_081,
            remote: Some("192.0.2.10:8080".parse().unwrap()),
            rx_queue: 5,
        }
    );
    assert_eq!(dump.sockets[&1111].remote, None, "a listener has no peer");
    assert_eq!(
        dump.sockets[&3333].remote,
        Some("[2001:db8::1]:443".parse().unwrap())
    );
    assert_eq!(
        dump.followed.len(),
        1,
        "loopback and close-wait are not followed"
    );

    let established = record(2, ESTABLISHED, (1, v4([192, 0, 2, 10]), 2), 7);
    for (datagram, fault) in [
        (message(20, 0x10, 9, &established), DumpFault::Interrupted),
        (
            message(20, 0, 8, &established),
            DumpFault::Torn("a message of another request"),
        ),
        (
            message(2, 0, 9, &(-16i32).to_ne_bytes()),
            DumpFault::Refused(16),
        ),
        (
            message(3, 0, 9, &(-16i32).to_ne_bytes()),
            DumpFault::Refused(16),
        ),
        (
            message(20, 0, 9, &established[..40]),
            DumpFault::Torn("a socket record cut short"),
        ),
        (
            message(20, 0, 9, &record(1, 10, (0, [0; 16], 0), 7)),
            DumpFault::Torn("a socket record of another family"),
        ),
        (Vec::new(), DumpFault::Torn("an empty datagram")),
    ] {
        assert_eq!(
            tcp_diag_records(&datagram, 9, &mut DumpRead::default()),
            Err(fault)
        );
    }
}

/// Past [`DUMP_KEEP`] connections a dump still counts each state, and keeps no more.
#[test]
fn a_dump_counts_every_record_and_keeps_the_first_4096() {
    let mut read = DumpRead::default();
    let mut datagram = Vec::new();
    for inode in 1..=u32::try_from(DUMP_KEEP).unwrap() + 3 {
        datagram.extend(message(
            20,
            0,
            1,
            &record(2, SYN_SENT, (1, v4([192, 0, 2, 1]), 2), inode),
        ));
    }
    datagram.extend(done(1));
    assert_eq!(tcp_diag_records(&datagram, 1, &mut read), Ok(Dump::Done));
    let dump = read.finish();
    assert_eq!(dump.counts.syn_sent, u32::try_from(DUMP_KEEP).unwrap() + 3);
    assert_eq!(dump.sockets.len(), DUMP_KEEP);
}

/// A RECORD WITH NO INODE TAKES NO PLACE, AND ONE FAMILY NEVER TAKES THE OTHER'S: past more
/// `TIME_WAIT` records than [`DUMP_KEEP`] and a full IPv4 share, an IPv6 connection is still kept
/// and followed. NEGATIVE CONTROL: the IPv4 connection past its family's share is not kept.
#[test]
fn a_record_with_no_inode_takes_no_place_and_one_family_never_takes_the_others() {
    let mut read = DumpRead::default();
    let mut datagram = Vec::new();
    for port in 0..u16::try_from(DUMP_KEEP).unwrap() + 100 {
        datagram.extend(message(
            20,
            0,
            1,
            &record(2, TIME_WAIT, (port, v4([192, 0, 2, 1]), 443), 0),
        ));
    }
    let share = u32::try_from(DUMP_KEEP).unwrap();
    for inode in 1..=share + 1 {
        datagram.extend(message(
            20,
            0,
            1,
            &record(2, ESTABLISHED, (1, v4([192, 0, 2, 1]), 443), inode),
        ));
    }
    datagram.extend(message(
        20,
        0,
        1,
        &record(
            10,
            ESTABLISHED,
            (2, v6([0x20, 1, 0x0d, 0xb8, 0, 0], 1), 443),
            999_999,
        ),
    ));
    datagram.extend(done(1));
    assert_eq!(tcp_diag_records(&datagram, 1, &mut read), Ok(Dump::Done));
    let dump = read.finish();
    assert!(
        dump.sockets.contains_key(&u64::from(share)),
        "IPv4's last kept"
    );
    assert!(
        !dump.sockets.contains_key(&u64::from(share + 1)),
        "IPv4 past its share"
    );
    assert_eq!(
        dump.sockets.get(&999_999).map(|r| r.state),
        Some(TcpState::Established),
        "IPv6 kept"
    );
    assert!(
        dump.followed
            .iter()
            .any(|f| f.prefix() == (false, [0x20, 1, 0x0d, 0xb8, 0, 0])),
        "IPv6 followed"
    );
}

/// A namespace's `net/tcp` table, which another namespace's process's sockets are read from, gives
/// each row's state, peer and queue, and skips a torn row.
#[test]
fn a_tcp_table_gives_state_peer_and_queue_and_skips_a_torn_row() {
    let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000   501        0 1111 1 0\n\
   1: 0100007F:C3A1 0A0200C0:1F90 01 00000000:00000000 00:00000000 00000000   501        0 2222 1 0\n\
   2: 0100007F:C3A2 0A0200C0:1F90 02 00000000:0000000";
    let rows = parse_net_tcp(table, false);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].state, TcpState::Listen);
    assert_eq!((rows[0].local_port, rows[0].remote), (8080, None));
    assert_eq!(rows[1].state, TcpState::Established);
    assert_eq!(rows[1].remote, Some("192.0.2.10:8080".parse().unwrap()));
    assert_eq!((rows[1].inode, rows[1].rx_queue), (2222, 0));
    let v6 = "  sl  local remote st q\n\
   0: 00000000000000000000000001000000:1F90 B80D0120000000000000000001000000:01BB 01 00000000:00000005 00:0 0 501 0 3333 1\n";
    let rows = parse_net_tcp(v6, true);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].remote, Some("[2001:db8::1]:443".parse().unwrap()));
    assert_eq!(rows[0].rx_queue, 5);
}

/// A FOLLOWED CONNECTION READS BY COOKIE, NOT BY DUMP (A3 D1): its lookup is no dump request and
/// sends back the socket id the dump wrote, cookie included, and asks for `INET_DIAG_INFO`. Its
/// answer gives the RTT; `ENOENT` says it is gone. NEGATIVE CONTROL: the dump request asks for a
/// dump (`NLM_F_DUMP`) and no extension.
#[test]
fn a_followed_connection_reads_by_cookie_not_by_dump() {
    let found = record(
        10,
        ESTABLISHED,
        (40_000, v6([0x20, 1, 0x0d, 0xb8, 0, 0], 1), 443),
        3333,
    );
    let mut read = DumpRead::default();
    let mut datagram = message(20, 0, 4, &found);
    datagram.extend(done(4));
    assert_eq!(tcp_diag_records(&datagram, 4, &mut read), Ok(Dump::Done));
    let followed = read.finish().followed[0];
    let lookup = lookup_request(&followed, 77);
    let flags = u16::from_ne_bytes([lookup[6], lookup[7]]);
    assert_eq!(flags, 0x1, "a request, no dump");
    assert_eq!(lookup[16], 10);
    assert_eq!(lookup[17], 6, "TCP");
    assert_eq!(lookup[18], 1 << 1, "INET_DIAG_INFO");
    assert_eq!(
        &lookup[24..72],
        &found[4..52],
        "the dump's socket id, cookie included"
    );
    let dump = dump_request(2, 78);
    assert_eq!(u16::from_ne_bytes([dump[6], dump[7]]), 0x301, "a dump");
    assert_eq!((dump[16], dump[18]), (2, 0), "IPv4, no extension");
    assert_eq!(&dump[20..24], &0xfffu32.to_ne_bytes(), "every state");

    let mut info = vec![0u8; 4 + 104];
    info[0..2].copy_from_slice(&108u16.to_ne_bytes());
    info[2..4].copy_from_slice(&2u16.to_ne_bytes());
    info[4 + 68..4 + 72].copy_from_slice(&21_300u32.to_ne_bytes());
    let mut answer = found.clone();
    answer.extend(info);
    assert_eq!(
        lookup_answer(&message(20, 0, 77, &answer), 77),
        Lookup::Rtt(21.3)
    );
    assert_eq!(
        lookup_answer(&message(20, 0, 76, &answer), 77),
        Lookup::Unread,
        "another lookup's answer"
    );
    assert_eq!(
        lookup_answer(&message(2, 0, 77, &(-2i32).to_ne_bytes()), 77),
        Lookup::Gone
    );
    assert_eq!(
        lookup_answer(&message(2, 0, 77, &(-1i32).to_ne_bytes()), 77),
        Lookup::Unread
    );
    assert_eq!(
        lookup_answer(&message(20, 0, 77, &found), 77),
        Lookup::Unread,
        "no INFO"
    );
    assert_eq!(median_ms(&mut [24.5, 12.5, 30.0, 1.0]), Some(18.5));
    assert_eq!(median_ms(&mut []), None);
}

/// FOLLOWED CONNECTIONS ARE AT MOST 64, ONE PER DESTINATION PREFIX FIRST (A3 ruling 11): a site's
/// many connections share one path, so the first to each IPv4 /24 or IPv6 /48 comes first, then
/// the rest in dump order. NEGATIVE CONTROL: with no prefix rule, the 70 connections to one /24
/// listed first would fill all 64.
#[test]
fn followed_connections_are_at_most_64_one_per_destination_prefix_first() {
    let mut read = DumpRead::default();
    let mut datagram = Vec::new();
    let mut inode = 0;
    let mut add = |family: u8, dst: [u8; 16]| {
        inode += 1;
        datagram.extend(message(
            20,
            0,
            1,
            &record(family, ESTABLISHED, (1000, dst, 443), inode),
        ));
    };
    for host in 0..70 {
        add(2, v4([192, 0, 2, host]));
    }
    add(2, v4([198, 51, 100, 7]));
    add(10, v6([0x20, 1, 0x0d, 0xb8, 0, 1], 1));
    add(10, v6([0x20, 1, 0x0d, 0xb8, 0, 1], 2));
    let mut mapped = [0u8; 16];
    mapped[10..12].copy_from_slice(&[0xff, 0xff]);
    mapped[12..16].copy_from_slice(&[203, 0, 113, 9]);
    add(10, mapped);
    datagram.extend(done(1));
    assert_eq!(tcp_diag_records(&datagram, 1, &mut read), Ok(Dump::Done));
    let followed = read.finish().followed;
    assert_eq!(followed.len(), FOLLOW_MAX);
    let prefixes: Vec<_> = followed.iter().map(Followed::prefix).collect();
    assert_eq!(prefixes[0], (true, [192, 0, 2, 0, 0, 0]));
    assert_eq!(prefixes[1], (true, [198, 51, 100, 0, 0, 0]));
    assert_eq!(prefixes[2], (false, [0x20, 1, 0x0d, 0xb8, 0, 1]));
    assert_eq!(
        prefixes[3],
        (true, [203, 0, 113, 0, 0, 0]),
        "IPv4-mapped is IPv4"
    );
    assert!(
        prefixes[4..]
            .iter()
            .all(|p| *p == (true, [192, 0, 2, 0, 0, 0]))
    );
}

/// THE TCP DUMP RUNS AT MOST ONCE PER TEN-SECOND EPOCH, AND A HIDDEN DECK'S AT MOST ONCE A MINUTE
/// (A3 D1): a dump is due at once, then only once the last try is an epoch old.
#[test]
fn the_tcp_dump_runs_at_most_once_per_ten_second_epoch_and_hidden_once_a_minute() {
    let pace = Pace::new();
    assert!(
        pace.due(None, false) && pace.due(None, true),
        "the first at once"
    );
    assert!(!pace.due(Some(Duration::from_millis(9_999)), false));
    assert!(pace.due(Some(EPOCH), false));
    assert!(
        !pace.due(Some(Duration::from_secs(59)), true),
        "hidden: a minute"
    );
    assert!(pace.due(Some(HIDDEN_EPOCH), true));
}

/// THE TCP DUMP RETIRES WHEN ITS P95 PASSES 15 MS: over its first 30 runs, a 95th percentile
/// past the lane bound retires it, at the second slow run, which decides it, and a retired dump
/// is never due. NEGATIVE CONTROL: one slow run in 30 is under the 95th percentile, and the dump
/// stays.
#[test]
fn the_tcp_dump_retires_when_its_p95_passes_15_ms() {
    let fast = Duration::from_micros(9_400);
    let slow = Duration::from_millis(16);
    let mut kept = Pace::new();
    for run in 0..SELF_CHECK_RUNS {
        kept.took(if run == 3 { slow } else { fast });
    }
    assert!(!kept.retired() && kept.due(None, false));
    let mut retired = Pace::new();
    for run in 0..SELF_CHECK_RUNS {
        retired.took(if run == 5 || run == 17 { slow } else { fast });
        assert_eq!(
            retired.retired(),
            run >= 17,
            "retired at the second slow run"
        );
    }
    assert_eq!(SLOW_RUNS_TO_RETIRE, 2);
    assert!(!retired.due(None, false), "never due once retired");
    for _ in 0..SELF_CHECK_RUNS {
        kept.took(slow);
    }
    assert!(!kept.retired(), "only the first 30 runs are judged");
}

/// ANOTHER NAMESPACE IS READ OFF THE READING'S PATH, AT MOST ONCE AN EPOCH, AND NEVER THRASHES: a
/// lookup only asks; the tables come due at once, then an epoch after their last read; a pass over
/// more namespaces than are held finds every one held each time, one asked for at the slowest
/// pass's pace (30 s) stays held, and one not asked for in [`NAMESPACE_HOLD`] is let go. NEGATIVE
/// CONTROL: the namespace past the cap stays unnamed rather than pushing out one held, which in a
/// pass over every namespace in turn would leave none found.
#[test]
fn other_namespaces_are_read_off_the_readings_path_at_most_once_an_epoch_and_never_thrash() {
    let t0 = Instant::now();
    let dir = Path::new("/proc/4711");
    let key = |n: u64| (PathBuf::from("/proc"), n);
    let past = u64::try_from(NAMESPACES_HELD).unwrap();
    let mut held = Namespaces::new();
    for n in 0..=past {
        assert_eq!(held.ask(key(n), dir, t0), None, "asked, not yet read");
    }
    let mut read = 0;
    while let Some((k, at)) = held.due(t0) {
        assert_eq!(at, dir);
        held.store(&k, Arc::new(HashMap::new()), t0);
        read += 1;
    }
    assert_eq!(read, NAMESPACES_HELD, "the one past the cap is not held");
    let t1 = t0 + Duration::from_secs(1);
    for n in 0..past {
        assert!(held.ask(key(n), dir, t1).is_some(), "{n} still held");
    }
    assert_eq!(held.ask(key(past), dir, t1), None, "past the cap: unnamed");
    assert_eq!(held.due(t1), None, "none due inside an epoch");
    assert!(
        held.due(t0 + EPOCH).is_some(),
        "due an epoch after its read"
    );
    let slow = t1 + Duration::from_secs(30);
    assert!(
        held.ask(key(0), dir, slow).is_some(),
        "asked for 30 s apart: still held"
    );
    let later = slow + NAMESPACE_HOLD;
    assert_eq!(held.ask(key(past), dir, later), None);
    assert_eq!(
        held.due(later).map(|(k, _)| k),
        Some(key(past)),
        "the rest let go, the newly asked taken in"
    );
}

/// A NAMESPACE IS READ ONLY WHEN ASKED FOR SINCE ITS LAST READ, NEVER AGAIN THROUGH A PROCESS
/// GONE, AND NOT AT ALL ONCE ITS READS RUN SLOW: one no pass asks for again is read no more and
/// let go even with no ask to let it go; a failed read keeps the sockets last read and waits for a
/// pass to ask through a live process; a second read past the lane's bound among the first
/// [`SELF_CHECK_RUNS`] lets every namespace go and takes none in. NEGATIVE CONTROL: the first slow
/// read retires nothing.
#[test]
fn a_namespace_is_read_only_when_asked_for_and_never_in_vain() {
    let t0 = Instant::now();
    let dir = Path::new("/proc/4711");
    let key = (PathBuf::from("/proc"), 7_u64);
    let mut held = Namespaces::new();
    assert_eq!(held.ask(key.clone(), dir, t0), None);
    assert!(held.due(t0).is_some(), "never read: due at once");
    held.store(&key, Arc::new(HashMap::new()), t0);
    assert_eq!(
        held.due(t0 + 2 * EPOCH),
        None,
        "not asked for since its read"
    );
    let t1 = t0 + 2 * EPOCH;
    assert!(held.ask(key.clone(), dir, t1).is_some());
    assert!(
        held.due(t1).is_some(),
        "asked for since a read an epoch ago"
    );
    held.failed(&key);
    assert_eq!(
        held.due(t1),
        None,
        "its process gone: not read through it again"
    );
    assert!(
        held.ask(key.clone(), Path::new("/proc/4712"), t1).is_some(),
        "the sockets last read stand"
    );
    assert_eq!(
        held.due(t1).map(|(_, at)| at),
        Some(PathBuf::from("/proc/4712")),
        "asked through a live process: read through it"
    );
    assert_eq!(
        held.due(t1 + NAMESPACE_HOLD),
        None,
        "asked for no more: let go"
    );
    assert_eq!(
        held.ask(key.clone(), dir, t1 + NAMESPACE_HOLD),
        None,
        "taken in anew"
    );

    let mut slow = Namespaces::new();
    let other = (PathBuf::from("/proc"), 8_u64);
    assert_eq!(slow.ask(key.clone(), dir, t0), None);
    slow.judge(LANE_BOUND + Duration::from_millis(1));
    assert!(slow.due(t0).is_some(), "one slow read retires nothing");
    slow.judge(LANE_BOUND);
    assert!(slow.due(t0).is_some(), "a read at the bound is not slow");
    slow.judge(LANE_BOUND + Duration::from_millis(1));
    assert_eq!(
        slow.due(t0),
        None,
        "a second slow read: every namespace let go"
    );
    assert_eq!(slow.ask(other.clone(), dir, t0), None);
    assert_eq!(slow.due(t0), None, "and none taken in");
}

/// On this machine: a dump names a listener this test holds by its inode, and a reading reads
/// the counters with the dump's listeners beside them.
#[cfg(target_os = "linux")]
#[test]
fn a_dump_on_this_machine_names_a_listener_this_test_holds() {
    use std::os::fd::AsRawFd as _;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
    let link =
        std::fs::read_link(format!("/proc/self/fd/{}", listener.as_raw_fd())).expect("own fd link");
    let inode: u64 = link
        .to_str()
        .and_then(|t| t.strip_prefix("socket:[")?.strip_suffix(']')?.parse().ok())
        .expect("a socket inode");
    dump_now();
    let dump = newest().expect("a dump");
    assert_eq!(
        dump.sockets.get(&inode).map(|r| r.state),
        Some(TcpState::Listen)
    );
    let summary = reading().expect("the counters read");
    assert!(summary.counts.listen >= 1, "{summary:?}");
}
