// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows arm's network decoders: pure functions over the tables the IP Helper fills
//! (`GetIfTable2`'s `MIB_IF_TABLE2`, `GetTcpTable2`'s and `GetTcp6Table2`'s connection tables,
//! `GetTcpStatisticsEx`'s `MIB_TCPSTATS`), so every host
//! compiles and tests them (design `docs/DESIGN-system-awareness-2026-10-16.md` §16;
//! `docs/DESIGN-stats-deck-flow-2026-11-02.md` §6.4, §6.5). The calls are in `win.rs`.

use crate::{LinkCounters, LinkKind, LinkReading, MAX_LINKS, TcpCounts, TcpSummary};

/// `sizeof(MIB_IF_ROW2)` (`netioapi.h`), 64-bit.
pub const IF_ROW2_SIZE: usize = 1352;
/// Where `MIB_IF_TABLE2`'s rows start: `NumEntries`, then padding to the rows' 8-byte alignment.
pub const IF_TABLE2_ROWS: usize = 8;

/// `MIB_IF_ROW2.InterfaceLuid`.
pub const IF_LUID: usize = 0;
/// `MIB_IF_ROW2.InterfaceIndex`.
pub const IF_INDEX: usize = 8;
/// `MIB_IF_ROW2.Alias`: 257 UTF-16 units, the name Settings shows (`Wi-Fi`, `Ethernet`).
pub const IF_ALIAS: usize = 28;
/// `MIB_IF_ROW2.Description`: 257 UTF-16 units, the adapter's own name.
pub const IF_DESCRIPTION: usize = 542;
/// `MIB_IF_ROW2.Type`: the IANA `ifType`.
pub const IF_TYPE: usize = 1128;
/// `MIB_IF_ROW2.InterfaceAndOperStatusFlags`: one byte of bits.
pub const IF_FLAGS: usize = 1152;
/// `MIB_IF_ROW2.OperStatus`.
pub const IF_OPER_STATUS: usize = 1156;
/// `MIB_IF_ROW2.TransmitLinkSpeed`, bits per second.
pub const IF_TX_SPEED: usize = 1192;
/// `MIB_IF_ROW2.ReceiveLinkSpeed`, bits per second.
pub const IF_RX_SPEED: usize = 1200;
/// `MIB_IF_ROW2.InOctets`, the first of the row's counters (each a `ULONG64`).
pub const IF_IN_OCTETS: usize = 1208;
/// `MIB_IF_ROW2.OutOctets`.
pub const IF_OUT_OCTETS: usize = 1280;

/// UTF-16 units in `Alias` and in `Description` (`IF_MAX_STRING_SIZE + 1`).
const IF_NAME_UNITS: usize = 257;
/// `InterfaceAndOperStatusFlags.FilterInterface`: a filter driver's layer over an adapter (a
/// packet scheduler, a firewall's lightweight filter), which repeats that adapter's counters.
const FLAG_FILTER: u8 = 1 << 1;
/// `InterfaceAndOperStatusFlags.ConnectorPresent`: a physical adapter (what `Get-NetAdapter
/// -Physical` keeps).
const FLAG_CONNECTOR: u8 = 1 << 2;
/// `IfOperStatusUp`.
const OPER_UP: u32 = 1;

/// `IF_TYPE_ETHERNET_CSMACD`.
pub const IF_TYPE_ETHERNET: u32 = 6;
/// `IF_TYPE_SOFTWARE_LOOPBACK`.
pub const IF_TYPE_LOOPBACK: u32 = 24;
/// `IF_TYPE_IEEE80211`.
pub const IF_TYPE_WIFI: u32 = 71;
/// `IF_TYPE_WWANPP` (a GSM-family mobile broadband modem).
pub const IF_TYPE_WWAN: u32 = 243;
/// `IF_TYPE_WWANPP2` (a CDMA-family one).
pub const IF_TYPE_WWAN2: u32 = 244;

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        b.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

/// The text of a fixed UTF-16LE field up to its first NUL.
fn wide_field(row: &[u8], at: usize) -> String {
    let Some(field) = row.get(at..at + IF_NAME_UNITS * 2) else {
        return String::new();
    };
    let (units, _) = field.as_chunks::<2>();
    let wide: Vec<u16> = units
        .iter()
        .map(|u| u16::from_le_bytes(*u))
        .take_while(|u| *u != 0)
        .collect();
    String::from_utf16_lossy(&wide)
}

/// What an IANA `ifType` is.
#[must_use]
pub const fn kind_of_type(if_type: u32) -> LinkKind {
    match if_type {
        IF_TYPE_ETHERNET => LinkKind::Wired,
        IF_TYPE_WIFI => LinkKind::Wifi,
        IF_TYPE_WWAN | IF_TYPE_WWAN2 => LinkKind::Cellular,
        _ => LinkKind::Other,
    }
}

/// A link speed as Windows states it: 0 and all ones both say "not known".
const fn speed(bps: u64) -> Option<u64> {
    match bps {
        0 | u64::MAX => None,
        known => Some(known),
    }
}

/// One direction's counters from the eight `ULONG64`s that start at `at` (octets, unicast
/// packets, non-unicast packets, discards, errors, …).
fn counters(row: &[u8], at: usize) -> LinkCounters {
    let n = |i: usize| u64_at(row, at + i * 8).unwrap_or(0);
    LinkCounters {
        bytes: n(0),
        packets: n(1).saturating_add(n(2)),
        drops: n(3),
        errors: n(4),
    }
}

/// One `MIB_IF_ROW2` as a reading; `None` for a filter driver's layer, whose counters are its
/// adapter's again. The name is the alias (the adapter's description where it has none); `up`
/// is `IfOperStatusUp`; `physical` is the connector bit; the rate is the receive link speed,
/// else the transmit one (they differ on Wi-Fi, and the reading holds one figure).
#[must_use]
pub fn link_of_row(row: &[u8]) -> Option<LinkReading> {
    if row.len() < IF_ROW2_SIZE {
        return None;
    }
    let flags = row[IF_FLAGS];
    if flags & FLAG_FILTER != 0 {
        return None;
    }
    let if_type = u32_at(row, IF_TYPE)?;
    let alias = wide_field(row, IF_ALIAS);
    Some(LinkReading {
        name: if alias.is_empty() {
            wide_field(row, IF_DESCRIPTION)
        } else {
            alias
        },
        index: u32_at(row, IF_INDEX)?,
        kind: kind_of_type(if_type),
        up: u32_at(row, IF_OPER_STATUS)? == OPER_UP,
        loopback: if_type == IF_TYPE_LOOPBACK,
        physical: Some(flags & FLAG_CONNECTOR != 0),
        rate_bps: speed(u64_at(row, IF_RX_SPEED)?).or_else(|| speed(u64_at(row, IF_TX_SPEED)?)),
        rx: counters(row, IF_IN_OCTETS),
        tx: counters(row, IF_OUT_OCTETS),
    })
}

/// How many rows a `MIB_IF_TABLE2` says it holds (`NumEntries`).
#[must_use]
pub fn if_table2_entries(head: &[u8]) -> Option<usize> {
    usize::try_from(u32_at(head, 0)?).ok()
}

/// The links of a `MIB_IF_TABLE2` (from its `NumEntries` on), in its order, at most
/// [`MAX_LINKS`]: the rows the buffer holds whole, less the filter layers ([`link_of_row`]).
/// `None` for a buffer too short to hold its count.
#[must_use]
pub fn if_table2(bytes: &[u8]) -> Option<Vec<LinkReading>> {
    let entries = if_table2_entries(bytes)?;
    let rows = bytes.get(IF_TABLE2_ROWS..).unwrap_or_default();
    let (rows, _) = rows.as_chunks::<IF_ROW2_SIZE>();
    Some(
        rows.iter()
            .take(entries)
            .filter_map(|row| link_of_row(row))
            .take(MAX_LINKS)
            .collect(),
    )
}

/// `sizeof(MIB_TCPSTATS)` (`tcpmib.h`): fifteen `DWORD`s.
pub const TCPSTATS_SIZE: usize = 60;
/// `MIB_TCPSTATS.dwCurrEstab`: connections now `ESTABLISHED` or `CLOSE_WAIT` (RFC 1213
/// `tcpCurrEstab`).
pub const TCPSTATS_CURR_ESTAB: usize = 32;

/// The established connections one family's `MIB_TCPSTATS` counts; `None` for a short buffer.
#[must_use]
pub fn tcp_established(stats: &[u8]) -> Option<u32> {
    if stats.len() < TCPSTATS_SIZE {
        return None;
    }
    u32_at(stats, TCPSTATS_CURR_ESTAB)
}

/// The machine's TCP summary from each family's established count ([`tcp_established`] of the
/// IPv4 and the IPv6 statistics), for a machine whose connection tables could not be read
/// ([`tcp_summary_of_tables`]): the two summed, with [`TcpSummary::counters_only`] set, since
/// the statistics hold no other state's count (the figure holds `CLOSE_WAIT`, as the kernel's
/// counter does) and no round-trip time. `None` when neither family answered.
#[must_use]
pub fn tcp_summary_of(v4: Option<u32>, v6: Option<u32>) -> Option<TcpSummary> {
    if v4.is_none() && v6.is_none() {
        return None;
    }
    Some(TcpSummary {
        counts: TcpCounts {
            established: v4.unwrap_or(0).saturating_add(v6.unwrap_or(0)),
            ..TcpCounts::default()
        },
        counters_only: true,
        srtt_ms: None,
    })
}

/// Where a `MIB_TCPTABLE2`'s or a `MIB_TCP6TABLE2`'s rows start (`tcpmib.h`): after
/// `dwNumEntries`; every field of a row is a `DWORD` or a byte array, so there is no padding.
pub const TCP_TABLE_ROWS: usize = 4;
/// `sizeof(MIB_TCPROW2)`: state, two addresses, two ports, owning pid, offload state.
pub const TCP_ROW2_SIZE: usize = 28;
/// `MIB_TCPROW2.dwState`.
pub const TCP_ROW2_STATE: usize = 0;
/// `sizeof(MIB_TCP6ROW2)`: two 16-byte addresses each with a scope id and a port, then the
/// state, the owning pid and the offload state.
pub const TCP6_ROW2_SIZE: usize = 60;
/// `MIB_TCP6ROW2.State`.
pub const TCP6_ROW2_STATE: usize = 48;

/// One family's connection table as counts by state: `table` is a `MIB_TCPTABLE2`
/// (`row_size` [`TCP_ROW2_SIZE`], `state_at` [`TCP_ROW2_STATE`]) or a `MIB_TCP6TABLE2`
/// ([`TCP6_ROW2_SIZE`], [`TCP6_ROW2_STATE`]), each row one socket in its `MIB_TCP_STATE`
/// (RFC 1213's `tcpConnState`: 1 closed, 2 listen, 3 SYN sent, 4 SYN received, 5 established,
/// 6 and 7 the FIN waits, 8 close wait, 9 closing, 10 last ACK, 11 time wait). A row in
/// `DELETE_TCB` (12) or a state the header does not name is a socket of no state, counted
/// nowhere. `None` for a table shorter than its own entry count says, which is no table.
#[must_use]
pub fn tcp_table_counts(table: &[u8], row_size: usize, state_at: usize) -> Option<TcpCounts> {
    let entries = usize::try_from(u32_at(table, 0)?).ok()?;
    let rows = table.get(TCP_TABLE_ROWS..)?;
    if row_size == 0 || rows.len() / row_size < entries {
        return None;
    }
    let mut counts = TcpCounts::default();
    for row in rows.chunks_exact(row_size).take(entries) {
        let slot = match u32_at(row, state_at)? {
            1 => &mut counts.close,
            2 => &mut counts.listen,
            3 => &mut counts.syn_sent,
            4 => &mut counts.syn_recv,
            5 => &mut counts.established,
            6 => &mut counts.fin_wait1,
            7 => &mut counts.fin_wait2,
            8 => &mut counts.close_wait,
            9 => &mut counts.closing,
            10 => &mut counts.last_ack,
            11 => &mut counts.time_wait,
            _ => continue,
        };
        *slot = slot.saturating_add(1);
    }
    Some(counts)
}

/// The machine's TCP summary from each family's connection table ([`tcp_table_counts`] of
/// `GetTcpTable2` and `GetTcp6Table2`): every state counted, the two families summed, so
/// [`TcpSummary::counters_only`] is clear and `established` holds no `CLOSE_WAIT`. A machine
/// with one family off reads the other's alone. Windows keeps a connection's round-trip time
/// only where an administrator turned its extended statistics on, so that stays unread. `None`
/// when neither table was read (the caller falls back to [`tcp_summary_of`]).
#[must_use]
pub fn tcp_summary_of_tables(v4: Option<TcpCounts>, v6: Option<TcpCounts>) -> Option<TcpSummary> {
    if v4.is_none() && v6.is_none() {
        return None;
    }
    let (a, b) = (v4.unwrap_or_default(), v6.unwrap_or_default());
    Some(TcpSummary {
        counts: TcpCounts {
            established: a.established.saturating_add(b.established),
            syn_sent: a.syn_sent.saturating_add(b.syn_sent),
            syn_recv: a.syn_recv.saturating_add(b.syn_recv),
            fin_wait1: a.fin_wait1.saturating_add(b.fin_wait1),
            fin_wait2: a.fin_wait2.saturating_add(b.fin_wait2),
            time_wait: a.time_wait.saturating_add(b.time_wait),
            close: a.close.saturating_add(b.close),
            close_wait: a.close_wait.saturating_add(b.close_wait),
            last_ack: a.last_ack.saturating_add(b.last_ack),
            listen: a.listen.saturating_add(b.listen),
            closing: a.closing.saturating_add(b.closing),
        },
        counters_only: false,
        srtt_ms: None,
    })
}

#[cfg(test)]
#[path = "win_net_tests.rs"]
mod tests;
