// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The routing socket's interface list (`sysctl {CTL_NET, PF_ROUTE, 0, 0, NET_RT_IFLIST2, 0}`), decoded from bytes: Darwin's 64-bit interface counters, which [`crate::net_links`] reads on macOS.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::machine::{
    LinkCounters, LinkKind, LinkReading, MAX_LINKS, TcpCounts, TcpSummary, kind_by_name,
    never_the_machines,
};

/// `CTL_NET` (`sys/sysctl.h`).
pub const CTL_NET: i32 = 4;
/// `PF_ROUTE` (`sys/socket.h`).
pub const PF_ROUTE: i32 = 17;
/// `NET_RT_IFLIST2` (`sys/socket.h`): the interface list with 64-bit counters.
pub const NET_RT_IFLIST2: i32 = 6;
/// `RTM_VERSION` (`net/route.h`): the message format decoded here; a message of another version is skipped.
pub const RTM_VERSION: u8 = 5;
/// `RTM_IFINFO2` (`net/route.h`): an interface's `if_msghdr2`.
pub const RTM_IFINFO2: u8 = 0x12;
/// `RTA_IFP` (`net/route.h`): the message carries the interface's link-level address.
pub const RTA_IFP: i32 = 0x10;
/// `AF_LINK` (`sys/socket.h`).
pub const AF_LINK: u8 = 18;
/// `IFF_UP` (`net/if.h`).
pub const IFF_UP: i32 = 0x1;
/// `IFF_LOOPBACK` (`net/if.h`).
pub const IFF_LOOPBACK: i32 = 0x8;
/// `IFF_RUNNING` (`net/if.h`).
pub const IFF_RUNNING: i32 = 0x40;
/// `SIOCGIFFUNCTIONALTYPE` (`sys/sockio.h`, `_IOWR('i', 173, struct ifreq)`): an interface's functional type, asked on any socket.
pub const SIOCGIFFUNCTIONALTYPE: u64 = 0xC020_69AD;
/// `IFRTYPE_FUNCTIONAL_WIRED` (`net/if.h`).
pub const IFRTYPE_FUNCTIONAL_WIRED: u32 = 2;
/// `IFRTYPE_FUNCTIONAL_WIFI_INFRA` (`net/if.h`).
pub const IFRTYPE_FUNCTIONAL_WIFI_INFRA: u32 = 3;
/// `IFRTYPE_FUNCTIONAL_CELLULAR` (`net/if.h`).
pub const IFRTYPE_FUNCTIONAL_CELLULAR: u32 = 5;

/// `struct if_data64`: 128 bytes at 4-byte alignment (`#pragma pack(4)`).
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub(crate) struct IfData64 {
    ifi_type: u8,
    ifi_typelen: u8,
    ifi_physical: u8,
    ifi_addrlen: u8,
    ifi_hdrlen: u8,
    ifi_recvquota: u8,
    ifi_xmitquota: u8,
    ifi_unused1: u8,
    ifi_mtu: u32,
    ifi_metric: u32,
    pub(crate) ifi_baudrate: u64,
    pub(crate) ifi_ipackets: u64,
    pub(crate) ifi_ierrors: u64,
    pub(crate) ifi_opackets: u64,
    pub(crate) ifi_oerrors: u64,
    ifi_collisions: u64,
    pub(crate) ifi_ibytes: u64,
    pub(crate) ifi_obytes: u64,
    ifi_imcasts: u64,
    ifi_omcasts: u64,
    pub(crate) ifi_iqdrops: u64,
    ifi_noproto: u64,
    ifi_recvtiming: u32,
    ifi_xmittiming: u32,
    /// `struct timeval32`: two `int32_t`.
    ifi_lastchange: [i32; 2],
}

/// `struct if_msghdr2`: 160 bytes at 4-byte alignment.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct IfMsghdr2 {
    pub(crate) ifm_msglen: u16,
    pub(crate) ifm_version: u8,
    pub(crate) ifm_type: u8,
    pub(crate) ifm_addrs: i32,
    pub(crate) ifm_flags: i32,
    pub(crate) ifm_index: u16,
    /// The two bytes C pads `ifm_index` with, named so the struct has no
    /// padding.
    ifm_pad: u16,
    ifm_snd_len: i32,
    ifm_snd_maxlen: i32,
    pub(crate) ifm_snd_drops: i32,
    ifm_timer: i32,
    pub(crate) ifm_data: IfData64,
}

/// `sizeof(struct if_msghdr2)`.
pub const MSGHDR2_SIZE: usize = 160;
const _: () = assert!(std::mem::size_of::<IfMsghdr2>() == MSGHDR2_SIZE);
const _: () = assert!(std::mem::size_of::<IfData64>() == 128);
const _: () = assert!(std::mem::offset_of!(IfMsghdr2, ifm_data) == 32);
const _: () = assert!(std::mem::offset_of!(IfData64, ifi_baudrate) == 16);
const _: () = assert!(std::mem::offset_of!(IfData64, ifi_ibytes) == 64);

/// `struct ifreq` as `SIOCGIFFUNCTIONALTYPE` reads it: the name, then the union's `ifru_functional_type`; 32 bytes, as the ioctl's size says.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub(crate) struct IfReqFunctional {
    pub(crate) ifr_name: [u8; 16],
    pub(crate) ifru_functional_type: u32,
    ifru_rest: [u8; 12],
}

const _: () = assert!(std::mem::size_of::<IfReqFunctional>() == 32);

/// `offsetof(struct sockaddr_dl, sdl_family)`.
pub const DL_FAMILY_AT: usize = 1;
/// `offsetof(struct sockaddr_dl, sdl_nlen)`: the name's length.
pub const DL_NLEN_AT: usize = 5;
/// `offsetof(struct sockaddr_dl, sdl_data)`: the name, then the address.
pub const DL_DATA_AT: usize = 8;

/// `INP_IPV4` (`netinet/in_pcb.h`).
pub const INP_IPV4: u8 = 0x1;
/// `INP_IPV6` (`netinet/in_pcb.h`).
pub const INP_IPV6: u8 = 0x2;
/// `TCPS_ESTABLISHED` (`netinet/tcp_fsm.h`).
pub const TCPS_ESTABLISHED: i32 = 4;
/// Fixed-point binary shift of Darwin's `xtcpcb64.t_srtt` (`TCP_RTT_SHIFT = 5`, 1/32 ms per unit).
pub const TCP_SRTT_SCALE: f32 = 32.0;
/// `sizeof(struct xinpgen)` (`netinet/in_pcb.h`).
pub const XINPGEN_SIZE: usize = 24;
/// `offsetof(struct xinpgen, xig_count)` (`netinet/in_pcb.h`).
pub const XINPGEN_COUNT_AT: usize = 4;
/// `sizeof(struct xsocket64)` (`sys/socketvar.h`).
pub const XSOCKET64_SIZE: usize = 108;
/// `sizeof(struct xtcpcb64)` (`netinet/tcp_var.h`).
pub const XTCPCB64_SIZE: usize = 472;
/// `offsetof(struct xtcpcb64, xt_inpcb.inp_vflag)`.
pub const XTCPCB64_VFLAG_AT: usize = 96;
/// `offsetof(struct xtcpcb64, xt_inpcb.inp_dependfaddr)` (16 bytes; IPv4 in its last four).
pub const XTCPCB64_FADDR_AT: usize = 100;
/// `offsetof(struct xtcpcb64, xt_inpcb.xi_socket.xso_len)`.
pub const XTCPCB64_XSO_LEN_AT: usize = 148;
/// `offsetof(struct xtcpcb64, t_state)`.
pub const XTCPCB64_STATE_AT: usize = 292;
/// `offsetof(struct xtcpcb64, t_srtt)`.
pub const XTCPCB64_SRTT_AT: usize = 392;
/// `offsetof(struct xtcpcb64, t_rttupdated)`.
pub const XTCPCB64_RTTUPDATED_AT: usize = 408;

/// Every layout row and constant above, as the C expression the SDK must agree with: `tests/iflist_sdk.rs` asserts each on `arm64` and `x86_64`.
#[cfg(target_os = "macos")]
pub const SDK_ROWS: &[(&str, i64)] = &[
    ("CTL_NET", CTL_NET as i64),
    ("PF_ROUTE", PF_ROUTE as i64),
    ("NET_RT_IFLIST2", NET_RT_IFLIST2 as i64),
    ("RTM_VERSION", RTM_VERSION as i64),
    ("RTM_IFINFO2", RTM_IFINFO2 as i64),
    ("RTA_IFP", RTA_IFP as i64),
    ("AF_LINK", AF_LINK as i64),
    ("IFF_UP", IFF_UP as i64),
    ("IFF_LOOPBACK", IFF_LOOPBACK as i64),
    ("IFF_RUNNING", IFF_RUNNING as i64),
    ("SIOCGIFFUNCTIONALTYPE", SIOCGIFFUNCTIONALTYPE as i64),
    ("IFRTYPE_FUNCTIONAL_WIRED", IFRTYPE_FUNCTIONAL_WIRED as i64),
    (
        "IFRTYPE_FUNCTIONAL_WIFI_INFRA",
        IFRTYPE_FUNCTIONAL_WIFI_INFRA as i64,
    ),
    (
        "IFRTYPE_FUNCTIONAL_CELLULAR",
        IFRTYPE_FUNCTIONAL_CELLULAR as i64,
    ),
    ("sizeof(struct if_msghdr2)", size_of::<IfMsghdr2>() as i64),
    (
        "offsetof(struct if_msghdr2, ifm_msglen)",
        std::mem::offset_of!(IfMsghdr2, ifm_msglen) as i64,
    ),
    (
        "offsetof(struct if_msghdr2, ifm_version)",
        std::mem::offset_of!(IfMsghdr2, ifm_version) as i64,
    ),
    (
        "offsetof(struct if_msghdr2, ifm_type)",
        std::mem::offset_of!(IfMsghdr2, ifm_type) as i64,
    ),
    (
        "offsetof(struct if_msghdr2, ifm_addrs)",
        std::mem::offset_of!(IfMsghdr2, ifm_addrs) as i64,
    ),
    (
        "offsetof(struct if_msghdr2, ifm_flags)",
        std::mem::offset_of!(IfMsghdr2, ifm_flags) as i64,
    ),
    (
        "offsetof(struct if_msghdr2, ifm_index)",
        std::mem::offset_of!(IfMsghdr2, ifm_index) as i64,
    ),
    (
        "offsetof(struct if_msghdr2, ifm_snd_len)",
        std::mem::offset_of!(IfMsghdr2, ifm_snd_len) as i64,
    ),
    (
        "offsetof(struct if_msghdr2, ifm_snd_drops)",
        std::mem::offset_of!(IfMsghdr2, ifm_snd_drops) as i64,
    ),
    ("offsetof(struct if_msghdr2, ifm_data)", DATA as i64),
    ("sizeof(struct if_data64)", size_of::<IfData64>() as i64),
    ("_Alignof(struct if_data64)", align_of::<IfData64>() as i64),
    (
        "offsetof(struct if_msghdr2, ifm_data.ifi_baudrate)",
        (DATA + std::mem::offset_of!(IfData64, ifi_baudrate)) as i64,
    ),
    (
        "offsetof(struct if_msghdr2, ifm_data.ifi_ipackets)",
        (DATA + std::mem::offset_of!(IfData64, ifi_ipackets)) as i64,
    ),
    (
        "offsetof(struct if_msghdr2, ifm_data.ifi_ierrors)",
        (DATA + std::mem::offset_of!(IfData64, ifi_ierrors)) as i64,
    ),
    (
        "offsetof(struct if_msghdr2, ifm_data.ifi_opackets)",
        (DATA + std::mem::offset_of!(IfData64, ifi_opackets)) as i64,
    ),
    (
        "offsetof(struct if_msghdr2, ifm_data.ifi_oerrors)",
        (DATA + std::mem::offset_of!(IfData64, ifi_oerrors)) as i64,
    ),
    (
        "offsetof(struct if_msghdr2, ifm_data.ifi_ibytes)",
        (DATA + std::mem::offset_of!(IfData64, ifi_ibytes)) as i64,
    ),
    (
        "offsetof(struct if_msghdr2, ifm_data.ifi_obytes)",
        (DATA + std::mem::offset_of!(IfData64, ifi_obytes)) as i64,
    ),
    (
        "offsetof(struct if_msghdr2, ifm_data.ifi_iqdrops)",
        (DATA + std::mem::offset_of!(IfData64, ifi_iqdrops)) as i64,
    ),
    (
        "offsetof(struct if_msghdr2, ifm_data.ifi_lastchange)",
        (DATA + std::mem::offset_of!(IfData64, ifi_lastchange)) as i64,
    ),
    ("sizeof(struct ifreq)", size_of::<IfReqFunctional>() as i64),
    (
        "offsetof(struct ifreq, ifr_ifru.ifru_functional_type)",
        std::mem::offset_of!(IfReqFunctional, ifru_functional_type) as i64,
    ),
    (
        "offsetof(struct sockaddr_dl, sdl_family)",
        DL_FAMILY_AT as i64,
    ),
    ("offsetof(struct sockaddr_dl, sdl_nlen)", DL_NLEN_AT as i64),
    ("offsetof(struct sockaddr_dl, sdl_data)", DL_DATA_AT as i64),
    ("INP_IPV4", INP_IPV4 as i64),
    ("INP_IPV6", INP_IPV6 as i64),
    ("TCPS_ESTABLISHED", TCPS_ESTABLISHED as i64),
    ("sizeof(struct xinpgen)", XINPGEN_SIZE as i64),
    (
        "offsetof(struct xinpgen, xig_count)",
        XINPGEN_COUNT_AT as i64,
    ),
    ("sizeof(struct xsocket64)", XSOCKET64_SIZE as i64),
    ("sizeof(struct xtcpcb64)", XTCPCB64_SIZE as i64),
    (
        "offsetof(struct xtcpcb64, xt_inpcb.inp_vflag)",
        XTCPCB64_VFLAG_AT as i64,
    ),
    (
        "offsetof(struct xtcpcb64, xt_inpcb.inp_dependfaddr)",
        XTCPCB64_FADDR_AT as i64,
    ),
    (
        "offsetof(struct xtcpcb64, xt_inpcb.xi_socket.xso_len)",
        XTCPCB64_XSO_LEN_AT as i64,
    ),
    (
        "offsetof(struct xtcpcb64, t_state)",
        XTCPCB64_STATE_AT as i64,
    ),
    ("offsetof(struct xtcpcb64, t_srtt)", XTCPCB64_SRTT_AT as i64),
    (
        "offsetof(struct xtcpcb64, t_rttupdated)",
        XTCPCB64_RTTUPDATED_AT as i64,
    ),
];

/// `offsetof(struct if_msghdr2, ifm_data)`, where the counters start.
#[cfg(target_os = "macos")]
const DATA: usize = std::mem::offset_of!(IfMsghdr2, ifm_data);

/// The interface name a `struct sockaddr_dl` holds: `sdl_data`'s first `sdl_nlen` bytes, held within `sdl_len` and `bytes`; `None` for another family, an empty name, or one that does not fit.
#[must_use]
pub fn dl_name(bytes: &[u8]) -> Option<String> {
    let len = usize::from(*bytes.first()?).min(bytes.len());
    if bytes.get(DL_FAMILY_AT) != Some(&AF_LINK) {
        return None;
    }
    let nlen = usize::from(*bytes.get(DL_NLEN_AT)?);
    let name = bytes[..len].get(DL_DATA_AT..DL_DATA_AT + nlen)?;
    (!name.is_empty()).then(|| String::from_utf8_lossy(name).into_owned())
}

/// A functional type (`SIOCGIFFUNCTIONALTYPE`) as a [`LinkKind`]: wired, Wi-Fi infrastructure and cellular name theirs; `IFRTYPE_FUNCTIONAL_UNKNOWN` (0) is `unknown`'s; every other type (loopback, AWDL, intcoproc, companion link, management) is other.
#[must_use]
pub const fn kind_of_functional(functional: u32, unknown: LinkKind) -> LinkKind {
    match functional {
        0 => unknown,
        IFRTYPE_FUNCTIONAL_WIRED => LinkKind::Wired,
        IFRTYPE_FUNCTIONAL_WIFI_INFRA => LinkKind::Wifi,
        IFRTYPE_FUNCTIONAL_CELLULAR => LinkKind::Cellular,
        _ => LinkKind::Other,
    }
}

/// The kind of the macOS link `name`: [`LinkKind::Other`], without asking, for a name [`crate::counts_toward_machine`] refuses by prefix (a loopback, tunnel, bridge, peer-to-peer or companion radio, coprocessor or guest link), since macOS reports `utun`, `bridge` and `anpi` links as wired and `llw` and `ap` links as Wi-Fi; else the functional type `functional` asks for ([`kind_of_functional`]), and the name's ([`kind_by_name`]) where it answers none or unknown.
#[must_use]
pub fn link_kind(name: &str, functional: impl FnOnce() -> Option<u32>) -> LinkKind {
    if never_the_machines(name) {
        return LinkKind::Other;
    }
    let by_name = kind_by_name(name);
    functional().map_or(by_name, |t| kind_of_functional(t, by_name))
}

/// What one ask of the kernel answered ([`read_growing`]).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Asked {
    /// It fit: this many bytes written, or, asked with no buffer, needed.
    Bytes(usize),
    /// The buffer was too small (`ENOMEM`).
    TooSmall,
    /// Any other failure.
    Failed,
}

/// A kernel answer of unknown size read by `ask` into `buf`, the caller's scratch: `ask(Some(buf))` fills `buf`, `ask(None)` names the size the answer needs now.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn read_growing(
    first: usize,
    cap: usize,
    buf: &mut Vec<u8>,
    mut ask: impl FnMut(Option<&mut [u8]>) -> Asked,
) -> Option<usize> {
    if first > cap {
        return None;
    }
    let mut size = buf.len().clamp(first, cap);
    for _ in 0..3 {
        if buf.len() < size {
            buf.resize(size, 0);
        }
        match ask(Some(&mut buf[..size])) {
            Asked::Bytes(n) => return Some(n.min(size)),
            Asked::TooSmall => {}
            Asked::Failed => return None,
        }
        let Asked::Bytes(need) = ask(None) else {
            return None;
        };
        size = need.saturating_add(need / 4).max(size.saturating_mul(2));
        if size > cap {
            return None;
        }
    }
    None
}

/// One interface's reading from its message header and name: up and
/// running from `IFF_UP | IFF_RUNNING`, loopback from `IFF_LOOPBACK`, the
/// rate where it is not zero, the send-queue drop count (a signed 32-bit
/// `int` in the kernel, the one 32-bit counter) read unsigned, so it wraps
/// at 2³² rather than reading zero past 2³¹; the kind is the name's
/// ([`kind_by_name`]) and `physical` is unknown.
fn link_of(h: &IfMsghdr2, name: String) -> LinkReading {
    let d = h.ifm_data;
    let up = IFF_UP | IFF_RUNNING;
    LinkReading {
        kind: kind_by_name(&name),
        name,
        index: u32::from(h.ifm_index),
        up: h.ifm_flags & up == up,
        loopback: h.ifm_flags & IFF_LOOPBACK != 0,
        physical: None,
        rate_bps: Some(d.ifi_baudrate).filter(|b| *b > 0),
        rx: LinkCounters {
            bytes: d.ifi_ibytes,
            packets: d.ifi_ipackets,
            errors: d.ifi_ierrors,
            drops: d.ifi_iqdrops,
        },
        tx: LinkCounters {
            bytes: d.ifi_obytes,
            packets: d.ifi_opackets,
            errors: d.ifi_oerrors,
            drops: u64::from(h.ifm_snd_drops.cast_unsigned()),
        },
    }
}

/// Every interface in a `NET_RT_IFLIST2` answer, in the kernel's order, at most [`MAX_LINKS`].
#[must_use]
pub fn links_of(buf: &[u8]) -> Vec<LinkReading> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while out.len() < MAX_LINKS {
        let Some(lead) = buf.get(at..at + 4) else {
            break;
        };
        let len = usize::from(u16::from_ne_bytes([lead[0], lead[1]]));
        let Some(msg) = buf.get(at..at + len).filter(|_| len >= 4) else {
            break;
        };
        at += len;
        if msg[2] != RTM_VERSION || msg[3] != RTM_IFINFO2 || len < MSGHDR2_SIZE {
            continue;
        }
        // SAFETY: `msg` holds at least `MSGHDR2_SIZE` bytes, the size of `IfMsghdr2`, which is integers only (no padding, every bit pattern a value); the read is unaligned.
        let h: IfMsghdr2 = unsafe { std::ptr::read_unaligned(msg.as_ptr().cast()) };
        let name = if h.ifm_addrs & RTA_IFP != 0 {
            dl_name(&msg[MSGHDR2_SIZE..])
        } else {
            None
        };
        out.push(link_of(&h, name.unwrap_or_default()));
    }
    out
}

/// Whether `rec`'s foreign address in `xtcpcb64` names a non-loopback, non-unspecified remote host.
fn pcb_remote_peer(rec: &[u8]) -> bool {
    let Some(&vflag) = rec.get(XTCPCB64_VFLAG_AT) else {
        return false;
    };
    let ip = if vflag & INP_IPV4 != 0 {
        let Ok(a): Result<[u8; 4], _> = rec
            .get(XTCPCB64_FADDR_AT + 12..XTCPCB64_FADDR_AT + 16)
            .unwrap_or_default()
            .try_into()
        else {
            return false;
        };
        IpAddr::V4(Ipv4Addr::from(a))
    } else if vflag & INP_IPV6 != 0 {
        let Ok(a): Result<[u8; 16], _> = rec
            .get(XTCPCB64_FADDR_AT..XTCPCB64_FADDR_AT + 16)
            .unwrap_or_default()
            .try_into()
        else {
            return false;
        };
        IpAddr::V6(Ipv6Addr::from(a))
    } else {
        return false;
    };
    match ip {
        IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_unspecified(),
        IpAddr::V6(v6) => {
            !v6.is_loopback()
                && !v6.is_unspecified()
                && !v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| v4.is_loopback() || v4.is_unspecified())
        }
    }
}

/// Decode Darwin's `net.inet.tcp.pcblist64` buffer into [`TcpSummary`] (state counts plus passive median smoothed RTT across established remote peers).
///
/// On macOS without Full Disk Access, XNU's `sysctl_pcblist64` redacts other processes' PCBs while
/// leaving the kernel's true `tcbinfo.ipi_count` in both `xinpgen.xig_count` records; when the
/// emitted records are a tiny fraction of `xig_count`, this returns `None` rather than falsely
/// reporting zero machine TCP connections.
#[must_use]
pub fn tcp_summary_of_pcblist64(buf: &[u8]) -> Option<TcpSummary> {
    if buf.len() < XINPGEN_SIZE * 2 {
        return None;
    }
    let head_len = u32::from_ne_bytes(buf.get(0..4)?.try_into().ok()?) as usize;
    if head_len < XINPGEN_SIZE || !head_len.is_multiple_of(4) || head_len > buf.len() {
        return None;
    }
    let head_count = u32::from_ne_bytes(
        buf.get(XINPGEN_COUNT_AT..XINPGEN_COUNT_AT + 4)?
            .try_into()
            .ok()?,
    );
    let mut counts = TcpCounts::default();
    let mut rtts: Vec<f32> = Vec::new();
    let mut at = head_len;
    let mut records = 0u32;
    let mut trailer_count = 0u32;
    let mut saw_trailer = false;
    while at < buf.len() {
        let len = u32::from_ne_bytes(buf.get(at..at + 4)?.try_into().ok()?) as usize;
        if len < XINPGEN_SIZE || !len.is_multiple_of(4) || at.checked_add(len)? > buf.len() {
            return None;
        }
        let rec = &buf[at..at + len];
        at += len;
        if len == head_len && at == buf.len() {
            trailer_count = u32::from_ne_bytes(
                rec.get(XINPGEN_COUNT_AT..XINPGEN_COUNT_AT + 4)?
                    .try_into()
                    .ok()?,
            );
            saw_trailer = true;
            break;
        }
        if len < XTCPCB64_STATE_AT + 4 {
            return None;
        }
        let xso_len = u32::from_ne_bytes(
            rec.get(XTCPCB64_XSO_LEN_AT..XTCPCB64_XSO_LEN_AT + 4)?
                .try_into()
                .ok()?,
        ) as usize;
        if xso_len != 0 && xso_len != XSOCKET64_SIZE {
            return None;
        }
        records = records.saturating_add(1);
        let raw_state = i32::from_ne_bytes(
            rec.get(XTCPCB64_STATE_AT..XTCPCB64_STATE_AT + 4)?
                .try_into()
                .ok()?,
        );
        counts.record(crate::jobs::TcpState::from_darwin(raw_state));
        if raw_state == TCPS_ESTABLISHED
            && len >= XTCPCB64_RTTUPDATED_AT + 4
            && pcb_remote_peer(rec)
        {
            let srtt = i32::from_ne_bytes(
                rec.get(XTCPCB64_SRTT_AT..XTCPCB64_SRTT_AT + 4)?
                    .try_into()
                    .ok()?,
            );
            let updated = u32::from_ne_bytes(
                rec.get(XTCPCB64_RTTUPDATED_AT..XTCPCB64_RTTUPDATED_AT + 4)?
                    .try_into()
                    .ok()?,
            );
            if updated > 0 && srtt > 0 {
                let ms = (srtt as f32) / TCP_SRTT_SCALE;
                if ms.is_finite() && ms > 0.0 {
                    rtts.push(ms);
                }
            }
        }
    }
    if !saw_trailer {
        return None;
    }
    let min_count = head_count.min(trailer_count);
    if (records == 0 && min_count >= 4) || (min_count >= 8 && records.saturating_mul(4) < min_count)
    {
        return None;
    }
    rtts.sort_by(f32::total_cmp);
    let srtt_ms = (!rtts.is_empty()).then(|| {
        let n = rtts.len();
        (rtts[(n - 1) / 2] + rtts[n / 2]) * 0.5
    });
    Some(TcpSummary { counts, srtt_ms })
}

/// Decode Darwin's `net.inet.tcp.pcblist64` buffer into [`TcpCounts`].
#[must_use]
pub fn tcp_counts_of_pcblist64(buf: &[u8]) -> Option<TcpCounts> {
    tcp_summary_of_pcblist64(buf).map(|s| s.counts)
}

#[cfg(test)]
mod tests {
    use std::mem::offset_of;

    use super::*;

    /// The byte offset of an `if_data64` field inside the header.
    fn data_at(field: usize) -> usize {
        offset_of!(IfMsghdr2, ifm_data) + field
    }

    fn put(buf: &mut [u8], at: usize, bytes: &[u8]) {
        buf[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// An `RTM_IFINFO2` message as the kernel writes one: the header, then
    /// (when `name` is given) a `sockaddr_dl` holding it, padded to 4 bytes.
    fn ifinfo2(index: u16, flags: i32, name: Option<&str>, rx: u64, tx: u64, baud: u64) -> Vec<u8> {
        let mut dl = Vec::new();
        if let Some(name) = name {
            let mut sdl = vec![0u8; (DL_DATA_AT + name.len()).max(20)];
            sdl[0] = sdl.len() as u8;
            sdl[DL_FAMILY_AT] = AF_LINK;
            sdl[DL_NLEN_AT] = name.len() as u8;
            put(&mut sdl, DL_DATA_AT, name.as_bytes());
            dl = sdl;
        }
        let len = (MSGHDR2_SIZE + dl.len()).next_multiple_of(4);
        let mut m = vec![0u8; len];
        put(&mut m, 0, &(len as u16).to_ne_bytes());
        m[2] = RTM_VERSION;
        m[3] = RTM_IFINFO2;
        let addrs = if name.is_some() { RTA_IFP } else { 0 };
        put(
            &mut m,
            offset_of!(IfMsghdr2, ifm_addrs),
            &addrs.to_ne_bytes(),
        );
        put(
            &mut m,
            offset_of!(IfMsghdr2, ifm_flags),
            &flags.to_ne_bytes(),
        );
        put(
            &mut m,
            offset_of!(IfMsghdr2, ifm_index),
            &index.to_ne_bytes(),
        );
        put(
            &mut m,
            offset_of!(IfMsghdr2, ifm_snd_drops),
            &(-1i32).to_ne_bytes(),
        );
        let d = |f| data_at(f);
        put(
            &mut m,
            d(offset_of!(IfData64, ifi_baudrate)),
            &baud.to_ne_bytes(),
        );
        put(
            &mut m,
            d(offset_of!(IfData64, ifi_ipackets)),
            &11u64.to_ne_bytes(),
        );
        put(
            &mut m,
            d(offset_of!(IfData64, ifi_ierrors)),
            &2u64.to_ne_bytes(),
        );
        put(
            &mut m,
            d(offset_of!(IfData64, ifi_opackets)),
            &13u64.to_ne_bytes(),
        );
        put(
            &mut m,
            d(offset_of!(IfData64, ifi_oerrors)),
            &3u64.to_ne_bytes(),
        );
        put(
            &mut m,
            d(offset_of!(IfData64, ifi_ibytes)),
            &rx.to_ne_bytes(),
        );
        put(
            &mut m,
            d(offset_of!(IfData64, ifi_obytes)),
            &tx.to_ne_bytes(),
        );
        put(
            &mut m,
            d(offset_of!(IfData64, ifi_iqdrops)),
            &5u64.to_ne_bytes(),
        );
        put(&mut m, MSGHDR2_SIZE, &dl);
        m
    }

    /// Another message (an `RTM_NEWADDR`, 0xc) of `len` bytes.
    fn other(len: u16) -> Vec<u8> {
        let mut m = vec![0u8; usize::from(len)];
        put(&mut m, 0, &len.to_ne_bytes());
        m[2] = RTM_VERSION;
        m[3] = 0xc;
        m
    }

    #[test]
    fn the_interface_list_reads_each_interfaces_counters_and_name() {
        let up = IFF_UP | IFF_RUNNING;
        let mut buf = ifinfo2(1, up | IFF_LOOPBACK, Some("lo0"), 7, 7, 0);
        buf.extend(other(36));
        buf.extend(ifinfo2(
            16,
            up,
            Some("en0"),
            663_548_928_000,
            90_182_656,
            183_200_000,
        ));
        buf.extend(ifinfo2(17, IFF_UP, None, 1, 2, 0));
        let links = links_of(&buf);
        assert_eq!(links.len(), 3, "{links:?}");

        let lo = &links[0];
        assert_eq!((lo.name.as_str(), lo.index), ("lo0", 1));
        assert!(lo.up && lo.loopback && lo.rate_bps.is_none(), "{lo:?}");

        let en = &links[1];
        assert_eq!(
            (en.name.as_str(), en.index, en.kind),
            ("en0", 16, LinkKind::Wired)
        );
        assert!(en.up && !en.loopback && en.physical.is_none(), "{en:?}");
        assert_eq!(en.rate_bps, Some(183_200_000));
        assert_eq!(
            en.rx,
            LinkCounters {
                bytes: 663_548_928_000,
                packets: 11,
                errors: 2,
                drops: 5
            }
        );
        assert_eq!(
            en.tx,
            LinkCounters {
                bytes: 90_182_656,
                packets: 13,
                errors: 3,
                drops: u64::from(u32::MAX)
            },
            "the send queue's drop count is a 32-bit counter read unsigned: \
             -1 is 2³² − 1, not a reset to zero"
        );

        let nameless = &links[2];
        assert_eq!((nameless.name.as_str(), nameless.index), ("", 17));
        assert!(!nameless.up, "IFF_UP without IFF_RUNNING is not up");
    }

    /// NEGATIVE CONTROLS: a message of another version is skipped; a
    /// message that runs past the buffer, or one whose length is under its
    /// own lead, ends the walk with what came before it.
    #[test]
    fn the_interface_list_skips_other_versions_and_stops_at_a_torn_message() {
        let up = IFF_UP | IFF_RUNNING;
        let mut old = ifinfo2(2, up, Some("en1"), 1, 1, 0);
        old[2] = RTM_VERSION - 1;
        let mut buf = old;
        buf.extend(ifinfo2(3, up, Some("en2"), 1, 1, 0));
        let mut torn = ifinfo2(4, up, Some("en3"), 1, 1, 0);
        torn.truncate(MSGHDR2_SIZE - 1);
        buf.extend(torn);
        let names: Vec<String> = links_of(&buf).into_iter().map(|l| l.name).collect();
        assert_eq!(names, ["en2"]);

        let mut zero = ifinfo2(5, up, Some("en4"), 1, 1, 0);
        zero.extend([0, 0, RTM_VERSION, 0xc]);
        zero.extend(ifinfo2(6, up, Some("en5"), 1, 1, 0));
        let names: Vec<String> = links_of(&zero).into_iter().map(|l| l.name).collect();
        assert_eq!(names, ["en4"]);
        assert!(links_of(&[]).is_empty());
        assert!(links_of(&[1, 2]).is_empty());
    }

    #[test]
    fn a_link_level_address_gives_its_name_within_its_length() {
        let mut sdl = vec![0u8; 20];
        sdl[0] = 20;
        sdl[DL_FAMILY_AT] = AF_LINK;
        sdl[DL_NLEN_AT] = 6;
        put(&mut sdl, DL_DATA_AT, b"bridge");
        assert_eq!(dl_name(&sdl).as_deref(), Some("bridge"));
        let mut inet = sdl.clone();
        inet[DL_FAMILY_AT] = 2;
        assert_eq!(dl_name(&inet), None);
        let mut empty = sdl.clone();
        empty[DL_NLEN_AT] = 0;
        assert_eq!(dl_name(&empty), None);
        let mut short = sdl.clone();
        short[0] = 10;
        assert_eq!(dl_name(&short), None);
        assert_eq!(dl_name(&sdl[..12]), None);
        assert_eq!(dl_name(&[]), None);
    }

    #[test]
    fn a_functional_type_names_wired_wifi_and_cellular() {
        let by_name = LinkKind::Wired;
        assert_eq!(kind_of_functional(0, by_name), LinkKind::Wired);
        assert_eq!(kind_of_functional(1, by_name), LinkKind::Other);
        assert_eq!(kind_of_functional(2, by_name), LinkKind::Wired);
        assert_eq!(kind_of_functional(3, by_name), LinkKind::Wifi);
        assert_eq!(kind_of_functional(4, by_name), LinkKind::Other);
        assert_eq!(kind_of_functional(5, by_name), LinkKind::Cellular);
        assert_eq!(kind_of_functional(8, by_name), LinkKind::Other);
    }

    /// The kind rule: a link the machine filter refuses by name is other,
    /// and its functional type is never asked, whatever the OS would claim
    /// (the live M5 Max read wired for `utun4`, `bridge0` and `anpi0`–`2`,
    /// Wi-Fi for `llw0` and `ap1`); every other link takes its functional
    /// type, and its name's kind where the ask answers none or unknown.
    #[test]
    fn a_refused_links_kind_is_other_and_the_rest_take_their_functional_type() {
        let never = || -> Option<u32> { panic!("an excluded link's type is never asked") };
        for name in [
            "utun4", "bridge0", "anpi0", "anpi2", "llw0", "ap1", "awdl0", "lo0", "ipsec0", "ppp0",
            "gif0", "stf0", "nan0", "vmenet0",
        ] {
            assert_eq!(link_kind(name, never), LinkKind::Other, "{name}");
        }
        let wired = || Some(IFRTYPE_FUNCTIONAL_WIRED);
        let wifi = || Some(IFRTYPE_FUNCTIONAL_WIFI_INFRA);
        assert_eq!(link_kind("en0", wifi), LinkKind::Wifi);
        assert_eq!(link_kind("en7", wired), LinkKind::Wired);
        assert_eq!(
            link_kind("pdp_ip0", || Some(IFRTYPE_FUNCTIONAL_CELLULAR)),
            LinkKind::Cellular
        );
        assert_eq!(link_kind("en0", || None), LinkKind::Wired, "no answer");
        assert_eq!(link_kind("en0", || Some(0)), LinkKind::Wired, "unknown");
        assert_eq!(link_kind("en3", || Some(4)), LinkKind::Other, "AWDL");
        assert_eq!(
            link_kind("XHC20", wired),
            LinkKind::Wired,
            "a name no prefix refuses takes the OS's type"
        );
    }

    /// A kernel whose answer is `answer` and grows by `grows` bytes each
    /// time it is asked its size (the list changing between two asks),
    /// logging each ask's buffer length (`None`: the size ask).
    struct Kernel {
        answer: Vec<u8>,
        grows: usize,
        asks: Vec<Option<usize>>,
    }

    impl Kernel {
        fn new(len: usize, grows: usize) -> Self {
            Self {
                answer: (0..len).map(|i| i as u8).collect(),
                grows,
                asks: Vec::new(),
            }
        }

        fn ask(&mut self, buf: Option<&mut [u8]>) -> Asked {
            self.asks.push(buf.as_ref().map(|b| b.len()));
            let len = self.answer.len();
            match buf {
                None => {
                    self.answer.resize(len + self.grows, 0xab);
                    Asked::Bytes(len)
                }
                Some(b) if b.len() >= len => {
                    b[..len].copy_from_slice(&self.answer);
                    Asked::Bytes(len)
                }
                Some(_) => Asked::TooSmall,
            }
        }
    }

    /// The interface list's buffer: the first size where it fits; where it
    /// is too small, what the kernel then asks for and a quarter more, at
    /// least twice the last size, three tries; the answer cut to what the
    /// kernel wrote.
    #[test]
    fn the_interface_lists_buffer_grows_to_what_the_kernel_asks_for() {
        assert_eq!(
            read_growing(65, 64, &mut Vec::new(), |_| panic!(
                "an oversized first buffer is never asked"
            )),
            None,
            "the initial buffer obeys the cap too"
        );
        let read = |k: &mut Kernel, cap| {
            let mut buf = Vec::new();
            read_growing(64, cap, &mut buf, |b| k.ask(b)).map(|n| buf[..n].to_vec())
        };

        let mut fits = Kernel::new(40, 0);
        assert_eq!(read(&mut fits, 1024).as_deref(), Some(&fits.answer[..]));
        assert_eq!(fits.asks, [Some(64)]);

        let mut once = Kernel::new(200, 0);
        assert_eq!(read(&mut once, 1024).as_deref(), Some(&once.answer[..]));
        assert_eq!(once.asks, [Some(64), None, Some(250)], "200 and a quarter");

        let mut small = Kernel::new(100, 0);
        assert_eq!(read(&mut small, 1024).map(|b| b.len()), Some(100));
        assert_eq!(small.asks, [Some(64), None, Some(128)], "at least twice");

        let mut grew = Kernel::new(200, 40);
        let got = read(&mut grew, 1024).expect("a quarter more covers it");
        assert_eq!((got.len(), grew.asks.len()), (240, 3));

        let mut twice = Kernel::new(200, 100);
        assert_eq!(read(&mut twice, 1024).map(|b| b.len()), Some(400));
        assert_eq!(
            twice.asks,
            [Some(64), None, Some(250), None, Some(500)],
            "the third and last try"
        );

        let mut runaway = Kernel::new(200, 1000);
        assert_eq!(read(&mut runaway, 1 << 20), None, "three tries at most");
        assert_eq!(runaway.asks.len(), 6);

        let mut huge = Kernel::new(5000, 0);
        assert_eq!(read(&mut huge, 1024), None, "past the cap");
        assert_eq!(huge.asks, [Some(64), None], "nothing past the cap is made");

        assert_eq!(
            read_growing(64, 1024, &mut Vec::new(), |_| Asked::Failed),
            None
        );
        let size_fails = read_growing(64, 1024, &mut Vec::new(), |b| match b {
            Some(_) => Asked::TooSmall,
            None => Asked::Failed,
        });
        assert_eq!(size_fails, None);
    }

    /// A scratch the caller keeps is offered whole: the read after one that
    /// grew it asks no size and allocates nothing, and the answer is what
    /// the kernel wrote, inside a scratch that keeps the size it was
    /// offered at.
    #[test]
    fn a_kept_scratch_is_offered_whole_and_read_again_in_place() {
        let mut scratch = Vec::new();
        let mut first = Kernel::new(200, 0);
        let n = read_growing(64, 1024, &mut scratch, |b| first.ask(b));
        assert_eq!(n, Some(200));
        assert_eq!(first.asks, [Some(64), None, Some(250)]);
        assert_eq!(&scratch[..200], &first.answer[..]);
        assert_eq!(scratch.len(), 250, "the scratch keeps the size it grew to");

        let at = scratch.as_ptr();
        let mut again = Kernel::new(120, 0);
        let n = read_growing(64, 1024, &mut scratch, |b| again.ask(b));
        assert_eq!(n, Some(120));
        assert_eq!(again.asks, [Some(250)], "offered whole, no size asked");
        assert_eq!(&scratch[..120], &again.answer[..]);
        assert_eq!(
            (scratch.as_ptr(), scratch.len()),
            (at, 250),
            "read in place"
        );

        let mut long = vec![0u8; 2048];
        let mut capped = Kernel::new(40, 0);
        assert_eq!(
            read_growing(64, 1024, &mut long, |b| capped.ask(b)),
            Some(40)
        );
        assert_eq!(capped.asks, [Some(1024)], "never past the cap");
    }

    #[test]
    fn pcblist64_decodes_states_and_rejects_torn_records() {
        let xinpgen = || {
            let mut b = vec![0u8; XINPGEN_SIZE];
            put(&mut b, 0, &(XINPGEN_SIZE as u32).to_ne_bytes());
            b
        };
        let pcb = |state: i32, xso_len: u32| {
            let mut b = vec![0u8; XTCPCB64_SIZE];
            put(&mut b, 0, &(XTCPCB64_SIZE as u32).to_ne_bytes());
            put(&mut b, XTCPCB64_XSO_LEN_AT, &xso_len.to_ne_bytes());
            put(&mut b, XTCPCB64_STATE_AT, &state.to_ne_bytes());
            b
        };
        let mut buf = xinpgen();
        for (state, xso) in [
            (4, XSOCKET64_SIZE as u32),
            (4, XSOCKET64_SIZE as u32),
            (1, XSOCKET64_SIZE as u32),
            (2, XSOCKET64_SIZE as u32),
            (5, XSOCKET64_SIZE as u32),
            (10, 0),
        ] {
            buf.extend_from_slice(&pcb(state, xso));
        }
        buf.extend_from_slice(&xinpgen());
        let counts = tcp_counts_of_pcblist64(&buf).expect("valid pcblist64");
        assert_eq!(
            counts,
            TcpCounts {
                established: 2,
                listen: 1,
                syn_sent: 1,
                close_wait: 1,
                time_wait: 1,
                ..TcpCounts::default()
            }
        );
        assert_eq!(counts.total(), 6);

        // Missing trailer is rejected.
        let without_trailer = &buf[..buf.len() - XINPGEN_SIZE];
        assert_eq!(tcp_counts_of_pcblist64(without_trailer), None);

        // Bad xso_len is rejected.
        let mut bad_xso = xinpgen();
        bad_xso.extend_from_slice(&pcb(4, 99));
        bad_xso.extend_from_slice(&xinpgen());
        assert_eq!(tcp_counts_of_pcblist64(&bad_xso), None);
    }

    #[test]
    fn pcblist64_summary_takes_median_srtt_of_established_remote_peers_only() {
        let xinpgen = || {
            let mut b = vec![0u8; XINPGEN_SIZE];
            put(&mut b, 0, &(XINPGEN_SIZE as u32).to_ne_bytes());
            b
        };
        let v4_pcb = |state: i32, ip: [u8; 4], srtt: i32, updated: u32| {
            let mut b = vec![0u8; XTCPCB64_SIZE];
            put(&mut b, 0, &(XTCPCB64_SIZE as u32).to_ne_bytes());
            put(
                &mut b,
                XTCPCB64_XSO_LEN_AT,
                &(XSOCKET64_SIZE as u32).to_ne_bytes(),
            );
            b[XTCPCB64_VFLAG_AT] = INP_IPV4;
            put(&mut b, XTCPCB64_FADDR_AT + 12, &ip);
            put(&mut b, XTCPCB64_STATE_AT, &state.to_ne_bytes());
            put(&mut b, XTCPCB64_SRTT_AT, &srtt.to_ne_bytes());
            put(&mut b, XTCPCB64_RTTUPDATED_AT, &updated.to_ne_bytes());
            b
        };
        let v6_pcb = |state: i32, ip: [u8; 16], srtt: i32, updated: u32| {
            let mut b = vec![0u8; XTCPCB64_SIZE];
            put(&mut b, 0, &(XTCPCB64_SIZE as u32).to_ne_bytes());
            put(
                &mut b,
                XTCPCB64_XSO_LEN_AT,
                &(XSOCKET64_SIZE as u32).to_ne_bytes(),
            );
            b[XTCPCB64_VFLAG_AT] = INP_IPV6;
            put(&mut b, XTCPCB64_FADDR_AT, &ip);
            put(&mut b, XTCPCB64_STATE_AT, &state.to_ne_bytes());
            put(&mut b, XTCPCB64_SRTT_AT, &srtt.to_ne_bytes());
            put(&mut b, XTCPCB64_RTTUPDATED_AT, &updated.to_ne_bytes());
            b
        };

        let mut buf = xinpgen();
        // Remote established peers: 12.5 ms (400 / 32), 20.0 ms (640 / 32), 80.0 ms (2560 / 32) -> median 20.0 ms.
        buf.extend_from_slice(&v4_pcb(TCPS_ESTABLISHED, [1, 1, 1, 1], 400, 3));
        buf.extend_from_slice(&v6_pcb(
            TCPS_ESTABLISHED,
            Ipv6Addr::new(0x2606, 0x4700, 0, 0, 0, 0, 0, 0x1111).octets(),
            640,
            5,
        ));
        buf.extend_from_slice(&v4_pcb(TCPS_ESTABLISHED, [8, 8, 8, 8], 2560, 1));
        // Ignored rows: loopback v4, loopback v6, mapped loopback v6, unmeasured (updated=0), non-established (CLOSE_WAIT=5).
        buf.extend_from_slice(&v4_pcb(TCPS_ESTABLISHED, [127, 0, 0, 1], 32, 9));
        buf.extend_from_slice(&v6_pcb(
            TCPS_ESTABLISHED,
            Ipv6Addr::LOCALHOST.octets(),
            32,
            9,
        ));
        buf.extend_from_slice(&v6_pcb(
            TCPS_ESTABLISHED,
            Ipv4Addr::LOCALHOST.to_ipv6_mapped().octets(),
            32,
            9,
        ));
        buf.extend_from_slice(&v4_pcb(TCPS_ESTABLISHED, [9, 9, 9, 9], 9600, 0));
        buf.extend_from_slice(&v4_pcb(5, [9, 9, 9, 9], 9600, 4));
        buf.extend_from_slice(&xinpgen());

        let summary = tcp_summary_of_pcblist64(&buf).expect("valid summary");
        assert_eq!(summary.counts.established, 7);
        assert_eq!(summary.counts.close_wait, 1);
        assert_eq!(summary.srtt_ms, Some(20.0));

        // Loopback-only table yields `srtt_ms: None`.
        let mut lo_only = xinpgen();
        lo_only.extend_from_slice(&v4_pcb(TCPS_ESTABLISHED, [127, 0, 0, 1], 64, 2));
        lo_only.extend_from_slice(&xinpgen());
        let lo_summary = tcp_summary_of_pcblist64(&lo_only).expect("valid loopback summary");
        assert_eq!(lo_summary.counts.established, 1);
        assert_eq!(lo_summary.srtt_ms, None);

        // Privacy-redacted pcblist64 (XNU without Full Disk Access: `xig_count` names 33 kernel
        // PCBs, 0 or 1 own record emitted) returns `None` instead of lying `0 TCP`.
        let xinpgen_counted = |count: u32| {
            let mut b = xinpgen();
            put(&mut b, XINPGEN_COUNT_AT, &count.to_ne_bytes());
            b
        };
        let mut redacted_zero = xinpgen_counted(33);
        redacted_zero.extend_from_slice(&xinpgen_counted(33));
        assert_eq!(tcp_summary_of_pcblist64(&redacted_zero), None);

        let mut redacted_own = xinpgen_counted(34);
        redacted_own.extend_from_slice(&v4_pcb(TCPS_ESTABLISHED, [127, 0, 0, 1], 64, 2));
        redacted_own.extend_from_slice(&xinpgen_counted(34));
        assert_eq!(tcp_summary_of_pcblist64(&redacted_own), None);

        // Unredacted pcblist64 (`xig_count = 10`, `8` active PCBs emitted) parses cleanly.
        let mut unredacted = xinpgen_counted(10);
        for _ in 0..8 {
            unredacted.extend_from_slice(&v4_pcb(TCPS_ESTABLISHED, [1, 1, 1, 1], 640, 3));
        }
        unredacted.extend_from_slice(&xinpgen_counted(10));
        let ok = tcp_summary_of_pcblist64(&unredacted).expect("unredacted pcblist64");
        assert_eq!(ok.counts.established, 8);
        assert_eq!(ok.srtt_ms, Some(20.0));
    }
}
