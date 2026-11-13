// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the Windows network decoders on tables laid out at SDK offsets.

use super::*;

fn put_u32(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_u64(buf: &mut [u8], at: usize, v: u64) {
    buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
fn put_wide(buf: &mut [u8], at: usize, text: &str) {
    for (i, unit) in text.encode_utf16().enumerate() {
        buf[at + i * 2..at + i * 2 + 2].copy_from_slice(&unit.to_le_bytes());
    }
}

/// `InterfaceAndOperStatusFlags`: `HardwareInterface`, `FilterInterface`, `ConnectorPresent`.
const HARDWARE: u8 = 1;
const FILTER: u8 = 2;
const CONNECTOR: u8 = 4;

struct Row<'a> {
    index: u32,
    alias: &'a str,
    description: &'a str,
    if_type: u32,
    flags: u8,
    oper: u32,
    tx_speed: u64,
    rx_speed: u64,
    /// `InOctets`, `InUcastPkts`, `InNUcastPkts`, `InDiscards`, `InErrors`.
    rx: [u64; 5],
    /// `OutOctets`, `OutUcastPkts`, `OutNUcastPkts`, `OutDiscards`, `OutErrors`.
    tx: [u64; 5],
}

fn row(r: &Row<'_>) -> Vec<u8> {
    let mut b = vec![0u8; IF_ROW2_SIZE];
    put_u64(&mut b, IF_LUID, 0x0006_0080_0000_0000 + u64::from(r.index));
    put_u32(&mut b, IF_INDEX, r.index);
    put_wide(&mut b, IF_ALIAS, r.alias);
    put_wide(&mut b, IF_DESCRIPTION, r.description);
    // Mtu, before Type: a value no reading may show.
    put_u32(&mut b, IF_TYPE - 4, 1500);
    put_u32(&mut b, IF_TYPE, r.if_type);
    b[IF_FLAGS] = r.flags;
    put_u32(&mut b, IF_OPER_STATUS, r.oper);
    // AdminStatus, after OperStatus: up, whatever the link is.
    put_u32(&mut b, IF_OPER_STATUS + 4, 1);
    put_u64(&mut b, IF_TX_SPEED, r.tx_speed);
    put_u64(&mut b, IF_RX_SPEED, r.rx_speed);
    for (i, v) in r.rx.iter().enumerate() {
        put_u64(&mut b, IF_IN_OCTETS + i * 8, *v);
    }
    // InUnknownProtos and the three per-cast octet counts: markers no reading may show.
    for (i, marker) in (5..9).zip(0xBAD5_u64..) {
        put_u64(&mut b, IF_IN_OCTETS + i * 8, marker);
    }
    for (i, v) in r.tx.iter().enumerate() {
        put_u64(&mut b, IF_OUT_OCTETS + i * 8, *v);
    }
    b
}

fn table(entries: u32, rows: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![0u8; IF_TABLE2_ROWS];
    put_u32(&mut b, 0, entries);
    for r in rows {
        b.extend(r);
    }
    b
}

/// What a link shows of its row: name, kind, up, loopback, physical, rate.
type Shown<'a> = (&'a str, LinkKind, bool, bool, Option<bool>, Option<u64>);

fn wifi() -> Row<'static> {
    Row {
        index: 14,
        alias: "Wi-Fi",
        description: "Intel(R) Wi-Fi 6E AX211 160MHz",
        if_type: IF_TYPE_WIFI,
        flags: HARDWARE | CONNECTOR,
        oper: 1,
        tx_speed: 1_201_000_000,
        rx_speed: 960_700_000,
        rx: [9_000_000_000, 7_000_000, 30_000, 12, 3],
        tx: [800_000_000, 2_000_000, 500, 4, 1],
    }
}

/// The offsets the decoder reads are the SDK's (`netioapi.h`, `sizeof(MIB_IF_ROW2)` 1352): each
/// field follows the one before it at the alignment the header gives it.
#[test]
fn the_interface_row_has_its_sdk_layout() {
    let wide = 257 * 2;
    assert_eq!(IF_INDEX, IF_LUID + 8);
    // InterfaceGuid (16 bytes) sits between the index and the alias.
    assert_eq!(IF_ALIAS, IF_INDEX + 4 + 16);
    assert_eq!(IF_DESCRIPTION, IF_ALIAS + wide);
    // PhysicalAddressLength, two 32-byte addresses and Mtu sit between the names and Type.
    assert_eq!(IF_TYPE, IF_DESCRIPTION + wide + 4 + 32 + 32 + 4);
    // TunnelType, MediaType, PhysicalMediumType, AccessType and DirectionType follow Type.
    assert_eq!(IF_FLAGS, IF_TYPE + 6 * 4);
    assert_eq!(IF_OPER_STATUS, IF_FLAGS + 4);
    // AdminStatus, MediaConnectState, NetworkGuid, ConnectionType, then 8-byte alignment.
    assert_eq!(
        IF_TX_SPEED,
        (IF_OPER_STATUS + 4 + 4 + 4 + 16 + 4).next_multiple_of(8)
    );
    assert_eq!(IF_RX_SPEED, IF_TX_SPEED + 8);
    assert_eq!(IF_IN_OCTETS, IF_RX_SPEED + 8);
    assert_eq!(IF_OUT_OCTETS, IF_IN_OCTETS + 9 * 8);
    // Eight out counters, then OutQLen.
    assert_eq!(IF_ROW2_SIZE, IF_OUT_OCTETS + 8 * 8 + 8);
}

/// One adapter's row reads as its link: the alias as the name, the kind from the IANA type, up
/// from the operational status, the connector bit as physical, the receive speed as the rate,
/// and each direction's octets, packets (unicast plus the rest), discards and errors.
#[test]
fn an_adapter_row_reads_as_its_link() {
    let link = link_of_row(&row(&wifi())).expect("a link");
    assert_eq!(
        link,
        LinkReading {
            name: "Wi-Fi".to_owned(),
            index: 14,
            kind: LinkKind::Wifi,
            up: true,
            loopback: false,
            physical: Some(true),
            rate_bps: Some(960_700_000),
            rx: LinkCounters {
                bytes: 9_000_000_000,
                packets: 7_030_000,
                errors: 3,
                drops: 12,
            },
            tx: LinkCounters {
                bytes: 800_000_000,
                packets: 2_000_500,
                errors: 1,
                drops: 4,
            },
        }
    );
    assert!(crate::counts_toward_machine(&link));
    // A Windows link's name is an alias a person chose: one that starts like a Unix tunnel's
    // name is still the machine's there, and is not on a host whose names are the kernel's.
    let renamed = LinkReading {
        name: "apartment".to_owned(),
        ..link.clone()
    };
    assert_eq!(crate::counts_toward_machine(&renamed), cfg!(windows));
    assert_eq!(link_of_row(&row(&wifi())[..IF_ROW2_SIZE - 1]), None);
}

/// A table reads as its adapters, in its order. NEGATIVE CONTROL: the filter layers Windows
/// lists over an adapter (same type, same status, same counters) are not links, so a machine's
/// traffic is counted once; without the filter bit the same row is one.
#[test]
fn a_table_reads_its_adapters_and_drops_the_filter_layers() {
    let filter = Row {
        index: 21,
        alias: "Wi-Fi-WFP Native MAC Layer LightWeight Filter-0000",
        flags: HARDWARE | FILTER | CONNECTOR,
        ..wifi()
    };
    let loopback = Row {
        index: 1,
        alias: "Loopback Pseudo-Interface 1",
        description: "Software Loopback Interface 1",
        if_type: IF_TYPE_LOOPBACK,
        flags: 0,
        tx_speed: 1_073_741_824,
        rx_speed: 1_073_741_824,
        rx: [0; 5],
        tx: [0; 5],
        ..wifi()
    };
    let wired_down = Row {
        index: 7,
        alias: "",
        description: "Realtek USB GbE Family Controller",
        if_type: IF_TYPE_ETHERNET,
        oper: 2,
        tx_speed: u64::MAX,
        rx_speed: 0,
        rx: [0; 5],
        tx: [0; 5],
        ..wifi()
    };
    let tunnel = Row {
        index: 30,
        alias: "Work VPN",
        if_type: 131,
        flags: 0,
        tx_speed: 100_000_000,
        rx_speed: 0,
        ..wifi()
    };
    let cellular = Row {
        index: 40,
        alias: "Cellular",
        if_type: IF_TYPE_WWAN,
        oper: 5,
        ..wifi()
    };
    let rows = [
        row(&loopback),
        row(&wifi()),
        row(&filter),
        row(&wired_down),
        row(&tunnel),
        row(&cellular),
    ];
    let links = if_table2(&table(6, &rows)).expect("a table");
    let got: Vec<Shown<'_>> = links
        .iter()
        .map(|l| {
            (
                l.name.as_str(),
                l.kind,
                l.up,
                l.loopback,
                l.physical,
                l.rate_bps,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                "Loopback Pseudo-Interface 1",
                LinkKind::Other,
                true,
                true,
                Some(false),
                Some(1_073_741_824)
            ),
            (
                "Wi-Fi",
                LinkKind::Wifi,
                true,
                false,
                Some(true),
                Some(960_700_000)
            ),
            (
                "Realtek USB GbE Family Controller",
                LinkKind::Wired,
                false,
                false,
                Some(true),
                None
            ),
            (
                "Work VPN",
                LinkKind::Other,
                true,
                false,
                Some(false),
                Some(100_000_000)
            ),
            (
                "Cellular",
                LinkKind::Cellular,
                false,
                false,
                Some(true),
                Some(960_700_000)
            ),
        ]
    );
    let total: u64 = links
        .iter()
        .filter(|l| l.up && !l.loopback && l.physical == Some(true))
        .map(|l| l.rx.bytes)
        .sum();
    assert_eq!(total, 9_000_000_000, "the Wi-Fi adapter's bytes, once");

    let unfiltered = Row {
        flags: HARDWARE | CONNECTOR,
        ..filter
    };
    let with = if_table2(&table(2, &[row(&wifi()), row(&unfiltered)])).expect("a table");
    assert_eq!(
        with.len(),
        2,
        "the control: the same row without the filter bit"
    );
}

/// A table is read by its count and by the bytes it holds, whichever is less; a buffer too
/// short for the count reads nothing.
#[test]
fn a_table_is_bounded_by_its_count_and_its_bytes() {
    let rows = [row(&wifi()), row(&wifi()), row(&wifi())];
    let names = |bytes: &[u8]| if_table2(bytes).map(|links| links.len());
    assert_eq!(names(&table(3, &rows)), Some(3));
    assert_eq!(
        names(&table(2, &rows)),
        Some(2),
        "rows past the count are not read"
    );
    assert_eq!(
        names(&table(9, &rows)),
        Some(3),
        "a count past the buffer reads what is there"
    );
    let whole = table(3, &rows);
    assert_eq!(
        names(&whole[..whole.len() - 1]),
        Some(2),
        "a cut row is not read"
    );
    assert_eq!(names(&table(0, &[])), Some(0));
    assert_eq!(names(&[1, 0, 0]), None);
    assert_eq!(names(&[]), None);
    assert_eq!(if_table2_entries(&table(7, &[])), Some(7));
}

#[test]
fn an_iana_type_names_its_kind() {
    assert_eq!(kind_of_type(6), LinkKind::Wired);
    assert_eq!(kind_of_type(71), LinkKind::Wifi);
    assert_eq!(kind_of_type(243), LinkKind::Cellular);
    assert_eq!(kind_of_type(244), LinkKind::Cellular);
    for other in [0, 1, 23, 24, 53, 131, 144] {
        assert_eq!(kind_of_type(other), LinkKind::Other, "{other}");
    }
}

fn tcpstats(curr_estab: u32) -> Vec<u8> {
    let mut b = vec![0u8; TCPSTATS_SIZE];
    // The fields around dwCurrEstab: dwEstabResets before it, dwInSegs after.
    put_u32(&mut b, TCPSTATS_CURR_ESTAB - 4, 0xBAD0);
    put_u32(&mut b, TCPSTATS_CURR_ESTAB, curr_estab);
    put_u32(&mut b, TCPSTATS_CURR_ESTAB + 4, 0xBAD1);
    b
}

/// The machine's TCP summary is both families' established counts, marked as counters only.
/// NEGATIVE CONTROLS: a short buffer reads nothing, one family alone still answers, and neither
/// answering is no reading, never a zero count.
#[test]
fn the_tcp_statistics_read_as_established_connections() {
    // dwCurrEstab is the ninth of fifteen DWORDs.
    assert_eq!(TCPSTATS_CURR_ESTAB, 8 * 4);
    assert_eq!(TCPSTATS_SIZE, 15 * 4);
    assert_eq!(tcp_established(&tcpstats(41)), Some(41));
    assert_eq!(tcp_established(&tcpstats(41)[..TCPSTATS_SIZE - 1]), None);
    let both = tcp_summary_of(Some(41), Some(9)).expect("a summary");
    assert_eq!(both.counts.established, 50);
    assert_eq!(both.counts.total(), 50, "no other state is counted");
    assert!(both.counters_only);
    assert_eq!(both.srtt_ms, None);
    assert_eq!(
        tcp_summary_of(None, Some(9)).map(|s| s.counts.established),
        Some(9)
    );
    assert_eq!(
        tcp_summary_of(Some(u32::MAX), Some(9)).map(|s| s.counts.established),
        Some(u32::MAX)
    );
    assert_eq!(tcp_summary_of(None, None), None);
}

/// A connection table of `states`, one row each, laid out as the family's SDK row: marker
/// bytes fill every other field, so a decoder reading the wrong offset counts garbage.
fn tcp_table(states: &[u32], row_size: usize, state_at: usize) -> Vec<u8> {
    let mut b = vec![0xEE_u8; TCP_TABLE_ROWS + states.len() * row_size];
    put_u32(&mut b, 0, u32::try_from(states.len()).expect("a row count"));
    for (i, state) in states.iter().enumerate() {
        put_u32(&mut b, TCP_TABLE_ROWS + i * row_size + state_at, *state);
    }
    b
}

/// A connection table reads as its sockets by state, for either family's row layout, and the
/// two families sum into a summary that says every state was read. NEGATIVE CONTROLS: a
/// `DELETE_TCB` row and a state the header does not name are counted nowhere; a table cut
/// short of its entry count is no table, never a count of the rows that fit; with neither
/// table there is no summary; and the statistics' summary still says it read one state only.
#[test]
fn the_tcp_tables_read_as_sockets_by_state() {
    // MIB_TCPROW2: seven DWORDs, the state first. MIB_TCP6ROW2: the state after two
    // (16-byte address, scope id, port) triples.
    assert_eq!(TCP_ROW2_SIZE, 7 * 4);
    assert_eq!(TCP_ROW2_STATE, 0);
    assert_eq!(TCP6_ROW2_SIZE, 2 * (16 + 4 + 4) + 3 * 4);
    assert_eq!(TCP6_ROW2_STATE, 2 * (16 + 4 + 4));

    let states = [5, 5, 5, 2, 2, 11, 8, 1, 3, 4, 6, 7, 9, 10, 12, 0, 99];
    let want = TcpCounts {
        established: 3,
        listen: 2,
        time_wait: 1,
        close_wait: 1,
        close: 1,
        syn_sent: 1,
        syn_recv: 1,
        fin_wait1: 1,
        fin_wait2: 1,
        closing: 1,
        last_ack: 1,
    };
    let v4 = tcp_table(&states, TCP_ROW2_SIZE, TCP_ROW2_STATE);
    let v6 = tcp_table(&states, TCP6_ROW2_SIZE, TCP6_ROW2_STATE);
    assert_eq!(
        tcp_table_counts(&v4, TCP_ROW2_SIZE, TCP_ROW2_STATE),
        Some(want)
    );
    assert_eq!(
        tcp_table_counts(&v6, TCP6_ROW2_SIZE, TCP6_ROW2_STATE),
        Some(want)
    );
    assert_eq!(
        want.total(),
        14,
        "the three rows of no state are not sockets"
    );
    // The IPv6 table read with the IPv4 layout finds marker bytes where the states are.
    assert_ne!(
        tcp_table_counts(&v6, TCP_ROW2_SIZE, TCP_ROW2_STATE),
        Some(want)
    );
    assert_eq!(
        tcp_table_counts(&v4[..v4.len() - 1], TCP_ROW2_SIZE, TCP_ROW2_STATE),
        None
    );
    assert_eq!(tcp_table_counts(&[], TCP_ROW2_SIZE, TCP_ROW2_STATE), None);
    assert_eq!(tcp_table_counts(&v4, 0, 0), None);
    assert_eq!(
        tcp_table_counts(&tcp_table(&[], TCP_ROW2_SIZE, 0), TCP_ROW2_SIZE, 0),
        Some(TcpCounts::default()),
        "an empty table is a machine with no socket, which is a reading"
    );

    let both = tcp_summary_of_tables(Some(want), Some(want)).expect("a summary");
    assert_eq!(both.counts.established, 6);
    assert_eq!(both.counts.listen, 4);
    assert_eq!(both.counts.total(), 28);
    assert!(!both.counters_only);
    assert_eq!(both.srtt_ms, None);
    assert_eq!(
        tcp_summary_of_tables(None, Some(want)).map(|s| s.counts),
        Some(want),
        "a machine with IPv4 off"
    );
    assert_eq!(tcp_summary_of_tables(None, None), None);
    assert!(tcp_summary_of(Some(3), Some(3)).is_some_and(|s| s.counters_only));
}
