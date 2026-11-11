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
        if need > cap {
            return None;
        }
        size = need
            .saturating_add(need / 4)
            .max(size.saturating_mul(2))
            .min(cap);
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
        let Some(rest) = buf.get(at..) else {
            break;
        };
        if rest.len() < 4 {
            break;
        }
        let len = usize::from(u16::from_ne_bytes([rest[0], rest[1]]));
        let Some(msg) = rest.get(..len).filter(|_| len >= 4) else {
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
    Some(TcpSummary {
        counts,
        counters_only: false,
        srtt_ms,
    })
}

#[cfg(test)]
#[path = "iflist_tests.rs"]
mod tests;
