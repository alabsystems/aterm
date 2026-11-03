// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local interface facts for background retry decisions. No socket is connected:
//! even a UDP connect without traffic can ask for Local Network permission.

use std::ffi::CStr;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// An address on an interface that is up and is not loopback. The mask and
/// flags also distinguish a changed attachment whose address stayed the same.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InterfaceAddress {
    pub name: String,
    pub address: IpAddr,
    pub netmask: Option<IpAddr>,
    pub flags: u32,
}

/// Read local interface state without DNS, network traffic or a connection.
/// Results are sorted and deduplicated, so enumeration order cannot wake a retry.
pub fn interfaces() -> std::io::Result<Vec<InterfaceAddress>> {
    let mut head = std::ptr::null_mut();
    // SAFETY: getifaddrs initializes this head pointer on success.
    if unsafe { libc::getifaddrs(&raw mut head) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    struct List(*mut libc::ifaddrs);
    impl Drop for List {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: this head came from getifaddrs and is freed once.
                unsafe { libc::freeifaddrs(self.0) };
            }
        }
    }
    let list = List(head);
    // SAFETY: list owns the live getifaddrs allocation for the whole read.
    Ok(unsafe { collect(list.0) })
}

/// # Safety
/// `at` heads a live ifaddrs list with valid names and family-appropriate
/// addresses/masks. Nothing in the list is modified or freed during this read.
unsafe fn collect(mut at: *const libc::ifaddrs) -> Vec<InterfaceAddress> {
    let mut out = Vec::new();
    while !at.is_null() {
        // SAFETY: the caller supplies a live linked list.
        let node = unsafe { &*at };
        at = node.ifa_next;
        if node.ifa_flags & libc::IFF_UP as u32 == 0
            || node.ifa_flags & libc::IFF_LOOPBACK as u32 != 0
            || node.ifa_name.is_null()
            || node.ifa_addr.is_null()
        {
            continue;
        }
        // SAFETY: ifa_addr points to the address's readable sockaddr header.
        let family = i32::from(unsafe { (*node.ifa_addr).sa_family });
        // SAFETY: the caller provides the sockaddr appropriate to this family.
        let Some(address) = (unsafe { sockaddr_ip(node.ifa_addr, family) }) else {
            continue;
        };
        if address.is_unspecified() || address.is_loopback() {
            continue;
        }
        // SAFETY: getifaddrs supplies a live nul-terminated interface name.
        let name = unsafe { CStr::from_ptr(node.ifa_name) }
            .to_string_lossy()
            .into_owned();
        let netmask = if node.ifa_netmask.is_null() {
            None
        } else {
            // SAFETY: the mask has this address's family; on Darwin its sa_len
            // can be short, which sockaddr_ip reads within that length.
            unsafe { sockaddr_ip(node.ifa_netmask, family) }
        };
        out.push(InterfaceAddress {
            name,
            address,
            netmask,
            flags: node.ifa_flags,
        });
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// # Safety
/// `at` points to a live sockaddr for `family`. On Darwin only sa_len bytes
/// need be readable, including the header; missing mask bytes are zero.
unsafe fn sockaddr_ip(at: *const libc::sockaddr, family: i32) -> Option<IpAddr> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: the header and its advertised bytes are readable by contract.
        let bytes =
            unsafe { std::slice::from_raw_parts(at.cast::<u8>(), usize::from((*at).sa_len)) };
        let octet = |offset: usize| bytes.get(offset).copied().unwrap_or(0);
        match family {
            libc::AF_INET => {
                let start = std::mem::offset_of!(libc::sockaddr_in, sin_addr);
                Some(Ipv4Addr::from(std::array::from_fn::<_, 4, _>(|i| octet(start + i))).into())
            }
            libc::AF_INET6 => {
                let start = std::mem::offset_of!(libc::sockaddr_in6, sin6_addr);
                Some(Ipv6Addr::from(std::array::from_fn::<_, 16, _>(|i| octet(start + i))).into())
            }
            _ => None,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        match family {
            libc::AF_INET => {
                // SAFETY: family identifies a complete sockaddr_in by contract.
                let value = unsafe { std::ptr::read_unaligned(at.cast::<libc::sockaddr_in>()) };
                Some(Ipv4Addr::from(value.sin_addr.s_addr.to_ne_bytes()).into())
            }
            libc::AF_INET6 => {
                // SAFETY: family identifies a complete sockaddr_in6 by contract.
                let value = unsafe { std::ptr::read_unaligned(at.cast::<libc::sockaddr_in6>()) };
                Some(Ipv6Addr::from(value.sin6_addr.s6_addr).into())
            }
            _ => None,
        }
    }
}

/// This machine's default IPv4 gateway's hardware (link-layer) address, read from the kernel's
/// routing and neighbour tables with no packet sent: the route table names the default route's
/// gateway, and the ARP/NDP table its hardware address. `None` where there is no default route,
/// where the gateway is not an IPv4 next hop, or where the gateway has no neighbour entry yet.
/// On macOS only the unscoped primary route counts, and its neighbour entry must belong to
/// that same interface, even when another attached network uses the same gateway address.
/// The hardware address tells one network from another far more sharply than its subnet does:
/// two routers both numbering a LAN `192.168.1.0/24` have different hardware addresses, so a
/// fact kept against one does not stand on the other.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[must_use]
pub fn default_gateway_hw() -> Option<Box<[u8]>> {
    #[cfg(target_os = "macos")]
    {
        primary_gateway_hardware(
            &route_table(NET_RT_DUMP, 0)?,
            &route_table(NET_RT_FLAGS, RTF_LLINFO)?,
            interface_name,
        )
    }
    #[cfg(target_os = "linux")]
    {
        linux_default_routes(
            &crate::atomic_file::read_regular_text(
                std::path::Path::new("/proc/net/route"),
                1024 * 1024,
            )
            .ok()?,
            &crate::atomic_file::read_regular_text(
                std::path::Path::new("/proc/net/arp"),
                1024 * 1024,
            )
            .ok()?,
        )
        .primary?
        .gateway_hw
    }
}

/// `None` on a platform whose kernel neighbour tables this build does not read: the caller keeps
/// no discovery record there and looks afresh each launch.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[must_use]
pub fn default_gateway_hw() -> Option<Box<[u8]>> {
    None
}

/// One IPv4 default route (`0.0.0.0/0`) as the kernel holds it ([`default_routes`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultRoute {
    /// The interface the route leaves by (`en0`, `utun4`), named from the route's own interface
    /// index, so its gateway and its interface always come from one route.
    pub interface: String,
    /// Its IPv4 next hop; `None` for a route straight onto a link (a VPN's `utun`).
    pub gateway: Option<Ipv4Addr>,
    /// That next hop's hardware address in the ARP table, on that same interface; `None` before
    /// ARP resolves it (after a wake or a join) or with no IPv4 next hop.
    pub gateway_hw: Option<Box<[u8]>>,
}

/// This machine's IPv4 default routes, read from the kernel's routing and ARP tables with no
/// packet sent and no question asked ([`default_routes`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DefaultRoutes {
    /// The default route traffic takes: the first up default route that is not scoped to one
    /// interface (`RTF_IFSCOPE`). With two networks attached (Wi-Fi and Ethernet), macOS keeps a
    /// scoped default on each and one unscoped primary, and only the primary says where traffic
    /// goes, so a reader that took the first default of any kind would pair one network's
    /// gateway with the other's address as the table's order moved.
    pub primary: Option<DefaultRoute>,
    /// The scoped defaults (`RTF_IFSCOPE`), in table order: where a tunnel holds the primary,
    /// the physical network under it is the scoped default on another interface.
    pub scoped: Vec<DefaultRoute>,
}

/// This machine's IPv4 default routes ([`DefaultRoutes`]), read with no packet: on macOS from
/// `sysctl`'s routing and ARP tables, on Linux from `/proc/net/route` and `/proc/net/arp` (which
/// keep no scoped defaults). Empty where neither is read, or with no default route.
#[must_use]
pub fn default_routes() -> DefaultRoutes {
    #[cfg(target_os = "macos")]
    {
        let Some(table) = route_table(NET_RT_DUMP, 0) else {
            return DefaultRoutes::default();
        };
        let arp = route_table(NET_RT_FLAGS, RTF_LLINFO).unwrap_or_default();
        default_routes_of(&table, &arp, interface_name)
    }
    #[cfg(target_os = "linux")]
    {
        let Ok(route) = crate::atomic_file::read_regular_text(
            std::path::Path::new("/proc/net/route"),
            1024 * 1024,
        ) else {
            return DefaultRoutes::default();
        };
        let arp = crate::atomic_file::read_regular_text(
            std::path::Path::new("/proc/net/arp"),
            1024 * 1024,
        )
        .unwrap_or_default();
        linux_default_routes(&route, &arp)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        DefaultRoutes::default()
    }
}

/// `net/route.h`'s `RTF_IFSCOPE`: a route scoped to one interface, which traffic takes only when
/// it is bound to that interface.
#[cfg(target_os = "macos")]
const RTF_IFSCOPE: i32 = 0x0100_0000;

/// The primary default's gateway hardware, bound to the route's own interface.
#[cfg(target_os = "macos")]
fn primary_gateway_hardware(
    table: &[u8],
    arp: &[u8],
    name_of: impl Fn(u16) -> Option<String>,
) -> Option<Box<[u8]>> {
    default_routes_of(table, arp, name_of).primary?.gateway_hw
}

/// The name of the interface whose index is `index` (`if_indextoname`), `None` for none.
#[cfg(target_os = "macos")]
fn interface_name(index: u16) -> Option<String> {
    let mut buf = [0 as libc::c_char; libc::IF_NAMESIZE];
    // SAFETY: `buf` holds IF_NAMESIZE bytes, as if_indextoname(3) writes at most.
    let named = unsafe { libc::if_indextoname(u32::from(index), buf.as_mut_ptr()) };
    if named.is_null() {
        return None;
    }
    // SAFETY: if_indextoname wrote a NUL-terminated name into `buf`.
    let name = unsafe { CStr::from_ptr(buf.as_ptr()) };
    Some(name.to_string_lossy().into_owned()).filter(|name| !name.is_empty())
}

/// [`default_routes`] of a `NET_RT_DUMP` `table` and a `NET_RT_FLAGS`/`RTF_LLINFO` `arp` table,
/// each route's interface named by `name_of` from its index. A route whose interface has no
/// name is passed over: it cannot be told apart from another.
#[cfg(target_os = "macos")]
fn default_routes_of(
    table: &[u8],
    arp: &[u8],
    name_of: impl Fn(u16) -> Option<String>,
) -> DefaultRoutes {
    let mut routes = DefaultRoutes::default();
    for_each_route_at(table, |index, flags, addrs, block| {
        if flags & RTF_UP == 0 {
            return;
        }
        let (dst, gw) = dst_and_gateway(block, addrs);
        if !is_default_destination(dst) {
            return;
        }
        let Some(interface) = name_of(index) else {
            return;
        };
        let gateway = (flags & RTF_GATEWAY != 0)
            .then(|| sockaddr_ipv4(gw))
            .flatten();
        let gateway_hw = gateway.and_then(|gateway| gateway_hardware_on(arp, gateway, index));
        let route = DefaultRoute {
            interface,
            gateway,
            gateway_hw,
        };
        if flags & RTF_IFSCOPE != 0 {
            routes.scoped.push(route);
        } else if routes.primary.is_none() {
            routes.primary = Some(route);
        }
    });
    routes
}

/// [`for_each_route`] that also hands each message's interface index (`rtm_index`).
#[cfg(target_os = "macos")]
fn for_each_route_at(table: &[u8], mut f: impl FnMut(u16, i32, i32, &[u8])) {
    let header = std::mem::size_of::<RtMsghdr>();
    let index_at = std::mem::offset_of!(RtMsghdr, rtm_index);
    let mut at = 0;
    for_each_route(table, |flags, addrs, block| {
        // `for_each_route` walks the same messages in order, each `header` bytes before its
        // block, so the index is read from the message this block belongs to.
        let msg = &table[at..];
        let msglen = usize::from(u16::from_ne_bytes([msg[0], msg[1]]));
        let index = u16::from_ne_bytes([msg[index_at], msg[index_at + 1]]);
        debug_assert_eq!(msglen, header + block.len());
        at += msglen;
        f(index, flags, addrs, block);
    });
}

/// [`gateway_hardware`] restricted to the ARP entry on interface `index`: two networks whose
/// gateways share an address (`192.168.1.1` on Wi-Fi and on Ethernet) keep one entry each.
#[cfg(target_os = "macos")]
fn gateway_hardware_on(table: &[u8], gateway: Ipv4Addr, index: u16) -> Option<Box<[u8]>> {
    let mut hardware = None;
    for_each_route_at(table, |at, _flags, addrs, block| {
        if hardware.is_some() || at != index {
            return;
        }
        let (dst, gw) = dst_and_gateway(block, addrs);
        if sockaddr_ipv4(dst) == Some(gateway) {
            hardware = sockaddr_dl_hardware(gw);
        }
    });
    hardware
}

/// [`default_routes`] of `/proc/net/route` and `/proc/net/arp`: the first up default row with a
/// `/0` mask is the primary; Linux keeps no scoped defaults in this table.
#[cfg(any(test, target_os = "linux"))]
fn linux_default_routes(route: &str, arp: &str) -> DefaultRoutes {
    const RTF_UP: u32 = 0x1;
    const RTF_GATEWAY: u32 = 0x2;
    for line in route.lines().skip(1) {
        let mut column = line.split_whitespace();
        let (Some(iface), Some(destination), Some(gateway), Some(flags)) =
            (column.next(), column.next(), column.next(), column.next())
        else {
            continue;
        };
        let mask = column.nth(3);
        let hex = |s: &str, len: Option<usize>| {
            len.is_none_or(|len| s.len() == len) && s.bytes().all(|b| b.is_ascii_hexdigit())
        };
        if destination != "00000000"
            || !hex(flags, None)
            || !hex(gateway, Some(8))
            || mask.is_some_and(|m| m != "00000000")
        {
            continue;
        }
        let (Ok(flags), Ok(raw)) = (
            u32::from_str_radix(flags, 16),
            u32::from_str_radix(gateway, 16),
        ) else {
            continue;
        };
        if flags & RTF_UP == 0 {
            continue;
        }
        let gateway =
            (flags & RTF_GATEWAY != 0 && raw != 0).then(|| Ipv4Addr::from(raw.to_le_bytes()));
        let gateway_hw = gateway.and_then(|gateway| linux_neighbour_hardware(arp, gateway, iface));
        return DefaultRoutes {
            primary: Some(DefaultRoute {
                interface: iface.to_owned(),
                gateway,
                gateway_hw,
            }),
            scoped: Vec::new(),
        };
    }
    DefaultRoutes::default()
}

/// `sys/sysctl.h` and `net/route.h` values the first-party `libc` does not carry (it has
/// `PF_ROUTE`, `AF_INET` and `AF_LINK`), declared here with their headers as the existing
/// `PF_ROUTE` message-type constants above are.
#[cfg(target_os = "macos")]
const CTL_NET: libc::c_int = 4;
#[cfg(target_os = "macos")]
const NET_RT_DUMP: libc::c_int = 1;
#[cfg(target_os = "macos")]
const NET_RT_FLAGS: libc::c_int = 2;
#[cfg(target_os = "macos")]
const RTF_UP: i32 = 0x1;
#[cfg(target_os = "macos")]
const RTF_GATEWAY: i32 = 0x2;
#[cfg(target_os = "macos")]
const RTF_LLINFO: i32 = 0x400;
#[cfg(target_os = "macos")]
const RTF_HOST: i32 = 0x4;
#[cfg(target_os = "macos")]
const RTA_DST: i32 = 0x1;
#[cfg(target_os = "macos")]
const RTA_GATEWAY: i32 = 0x2;
#[cfg(target_os = "macos")]
const RTA_IFP: i32 = 0x10;
#[cfg(target_os = "macos")]
const RTA_IFA: i32 = 0x20;
/// `RTM_VERSION` and the `RTM_GET` message type (`net/route.h`).
#[cfg(target_os = "macos")]
const RTM_VERSION: u8 = 5;
#[cfg(target_os = "macos")]
const RTM_GET: u8 = 0x4;
/// The sockaddr slots a route message's `rtm_addrs` bitmask can name (`RTAX_MAX`, `net/route.h`).
#[cfg(target_os = "macos")]
const RTAX_MAX: usize = 8;

/// The fixed head of a `PF_ROUTE` message (`net/route.h` `struct rt_msghdr`), declared here for
/// its size and the offsets of the two fields [`for_each_route`] reads: the kernel lays a
/// message's sockaddrs out immediately after it. Plain Rust integer types match the C ABI on
/// both macOS targets, so `repr(C)` computes the same 92-byte layout the kernel wrote (the same
/// size the route-change test builds its headers at). The first-party `libc` does not carry the
/// struct.
#[cfg(target_os = "macos")]
#[repr(C)]
#[allow(dead_code)] // The fields exist for the ABI layout; the walk reads two by offset.
struct RtMsghdr {
    rtm_msglen: u16,
    rtm_version: u8,
    rtm_type: u8,
    rtm_index: u16,
    rtm_flags: i32,
    rtm_addrs: i32,
    rtm_pid: i32,
    rtm_seq: i32,
    rtm_errno: i32,
    rtm_use: i32,
    rtm_inits: u32,
    rtm_rmx: [u32; 14],
}

#[cfg(target_os = "macos")]
const _: () = assert!(std::mem::size_of::<RtMsghdr>() == 92);

/// A `PF_ROUTE` table dumped by `sysctl` with no packet: `which` is `NET_RT_DUMP` (the routing
/// table) or `NET_RT_FLAGS` (its entries whose flags include `flags`, e.g. `RTF_LLINFO` for the
/// ARP/NDP cache). `None` on any sysctl failure or an empty table; a table that grew between the
/// sizing call and the read fails the read, so the caller keeps nothing and looks again.
#[cfg(target_os = "macos")]
fn route_table(which: libc::c_int, flags: libc::c_int) -> Option<Vec<u8>> {
    let mut mib = [CTL_NET, libc::PF_ROUTE, 0, libc::AF_INET, which, flags];
    let count = mib.len() as libc::c_uint;
    let mut len = 0usize;
    // SAFETY: `mib` is six ints; a null buffer asks sysctl only for the size it would write.
    let sized = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            count,
            std::ptr::null_mut(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if sized != 0 || len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len];
    // SAFETY: `buf` holds `len` bytes; sysctl writes at most `len` and sets `len` to what it wrote.
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            count,
            buf.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if read != 0 {
        return None;
    }
    buf.truncate(len);
    Some(buf)
}

/// Call `f(flags, addrs, sockaddrs)` for each message in a `PF_ROUTE` table, walking by
/// `rtm_msglen`; a truncated or zero-length message ends the walk. The buffer may be unaligned,
/// so the two header fields the walk needs are read from their byte offsets.
#[cfg(target_os = "macos")]
fn for_each_route(table: &[u8], mut f: impl FnMut(i32, i32, &[u8])) {
    let header = std::mem::size_of::<RtMsghdr>();
    let flags_at = std::mem::offset_of!(RtMsghdr, rtm_flags);
    let addrs_at = std::mem::offset_of!(RtMsghdr, rtm_addrs);
    let mut at = 0;
    while at + header <= table.len() {
        let msg = &table[at..];
        let msglen = usize::from(u16::from_ne_bytes([msg[0], msg[1]]));
        if msglen < header || at + msglen > table.len() {
            break;
        }
        let read = |offset: usize| i32::from_ne_bytes(msg[offset..offset + 4].try_into().unwrap());
        f(read(flags_at), read(addrs_at), &msg[header..msglen]);
        at += msglen;
    }
}

/// The `RTA_DST` and `RTA_GATEWAY` sockaddrs of a route message's address block, selected from
/// the `addrs` bitmask in slot order with each sockaddr padded up to four bytes (`net/route.h`
/// `ROUNDUP`). A sockaddr whose padded length runs past the block is read for what is there.
#[cfg(target_os = "macos")]
fn dst_and_gateway(block: &[u8], addrs: i32) -> (Option<&[u8]>, Option<&[u8]>) {
    (
        route_sockaddr(block, addrs, RTA_DST),
        route_sockaddr(block, addrs, RTA_GATEWAY),
    )
}

/// The sockaddr the bit `want` names in a route message's address block, found as
/// [`dst_and_gateway`] finds its two: `None` where `addrs` does not name it, or the block ends
/// before it.
#[cfg(target_os = "macos")]
fn route_sockaddr(block: &[u8], addrs: i32, want: i32) -> Option<&[u8]> {
    let mut at = 0;
    for slot in 0..RTAX_MAX {
        let bit = 1i32 << slot;
        if addrs & bit == 0 {
            continue;
        }
        let &sa_len = block.get(at)?;
        let advance = if sa_len == 0 {
            4
        } else {
            (usize::from(sa_len) + 3) & !3
        };
        if bit == want {
            return Some(&block[at..(at + advance).min(block.len())]);
        }
        at += advance;
    }
    None
}

/// How long [`route_source_ipv4`] waits for the kernel's answer to its `RTM_GET`, which the
/// kernel queues before the write returns: a bound, never a pace.
#[cfg(target_os = "macos")]
const ROUTE_ANSWER_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

/// The local IPv4 address this Mac's kernel sends from toward `to`: the address of the route
/// `to` takes, as `route -n get` reads it (a `PF_ROUTE` `RTM_GET` that asks for the route's
/// interface, and so for its address, `RTA_IFA`). A UDP socket connected to `to` gets its
/// address from the same lookup, but on macOS that connect toward a local-network destination (a
/// LAN address, a multicast group such as mDNS's) can put macOS's Local Network question on
/// screen. A routing socket sends no packet and is no network socket, so it asks nothing. `None`
/// where no route takes `to`, the route has no IPv4 address but loopback's, or the kernel's
/// answer does not come.
#[cfg(target_os = "macos")]
#[must_use]
pub fn route_source_ipv4(to: Ipv4Addr) -> Option<Ipv4Addr> {
    use std::os::fd::FromRawFd as _;
    static SEQUENCE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
    let pid = i32::try_from(std::process::id()).ok()?;
    let sequence = SEQUENCE
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        .wrapping_add(1);
    let fd = crate::kept_out(|| {
        // SAFETY: socket(2) takes no pointer; a descriptor it returns is owned below.
        let raw = unsafe { libc::socket(libc::PF_ROUTE, libc::SOCK_RAW, libc::AF_INET) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `raw` is the fresh descriptor socket(2) returned, owned by nothing else.
        Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) })
    })
    .ok()?;
    let raw = std::os::fd::AsRawFd::as_raw_fd(&fd);
    let request = route_get_request(to, sequence);
    // SAFETY: `request` is live and readable for its whole length. A destination no route takes
    // fails the write (`ESRCH`).
    let wrote = unsafe { libc::write(raw, request.as_ptr().cast(), request.len()) };
    if usize::try_from(wrote).ok() != Some(request.len()) {
        return None;
    }
    let deadline = std::time::Instant::now() + ROUTE_ANSWER_WAIT;
    let mut buf = [0u8; 2048];
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return None;
        }
        let mut poll = libc::pollfd {
            fd: raw,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = libc::c_int::try_from(left.as_millis().max(1)).unwrap_or(libc::c_int::MAX);
        // SAFETY: `poll` is one live pollfd and the count passed is 1.
        let ready = unsafe { libc::poll(&raw mut poll, 1, millis) };
        if ready < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return None;
        }
        if ready <= 0 {
            continue;
        }
        // SAFETY: `buf` is live and writable for its whole length.
        let read = unsafe { libc::recv(raw, buf.as_mut_ptr().cast(), buf.len(), 0) };
        let Ok(read) = usize::try_from(read) else {
            return None;
        };
        if let Some(answer) = route_get_answer(&buf[..read], pid, sequence) {
            return answer;
        }
    }
}

/// The `RTM_GET` [`route_source_ipv4`] writes for `to` as `sequence`: the head, `to` as the
/// destination, and an empty link-layer sockaddr in the interface's slot, which asks the kernel to
/// name the route's interface and its address in the answer, as `route -n get` asks.
#[cfg(target_os = "macos")]
fn route_get_request(to: Ipv4Addr, sequence: i32) -> Vec<u8> {
    let header = std::mem::size_of::<RtMsghdr>();
    let mut destination = [0u8; std::mem::size_of::<libc::sockaddr_in>()];
    destination[0] = destination.len() as u8;
    destination[1] = libc::AF_INET as u8;
    destination[4..8].copy_from_slice(&to.octets());
    let mut interface = [0u8; std::mem::size_of::<libc::sockaddr_dl>()];
    interface[0] = interface.len() as u8;
    interface[1] = libc::AF_LINK as u8;
    let mut message = vec![0u8; header];
    let length = (header + destination.len() + interface.len()) as u16;
    message[0..2].copy_from_slice(&length.to_ne_bytes());
    message[std::mem::offset_of!(RtMsghdr, rtm_version)] = RTM_VERSION;
    message[std::mem::offset_of!(RtMsghdr, rtm_type)] = RTM_GET;
    let mut put = |offset: usize, value: i32| {
        message[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
    };
    put(std::mem::offset_of!(RtMsghdr, rtm_flags), RTF_UP | RTF_HOST);
    put(std::mem::offset_of!(RtMsghdr, rtm_addrs), RTA_DST | RTA_IFP);
    put(std::mem::offset_of!(RtMsghdr, rtm_seq), sequence);
    message.extend_from_slice(&destination);
    message.extend_from_slice(&interface);
    message
}

/// What one message a routing socket read says to the `RTM_GET` process `pid` wrote as
/// `sequence` ([`route_get_request`]): `None` when it answers something else (another process's
/// lookup, another of this one's, a change of a route), else the address of the route found
/// (`RTA_IFA`), `Some(None)` where the kernel found none or that address is no usable IPv4 one.
#[cfg(target_os = "macos")]
fn route_get_answer(message: &[u8], pid: i32, sequence: i32) -> Option<Option<Ipv4Addr>> {
    let header = std::mem::size_of::<RtMsghdr>();
    let msglen = usize::from(u16::from_ne_bytes([*message.first()?, *message.get(1)?]));
    if msglen < header || msglen > message.len() {
        return None;
    }
    let read = |offset: usize| i32::from_ne_bytes(message[offset..offset + 4].try_into().unwrap());
    if message[std::mem::offset_of!(RtMsghdr, rtm_type)] != RTM_GET
        || read(std::mem::offset_of!(RtMsghdr, rtm_pid)) != pid
        || read(std::mem::offset_of!(RtMsghdr, rtm_seq)) != sequence
    {
        return None;
    }
    if read(std::mem::offset_of!(RtMsghdr, rtm_errno)) != 0 {
        return Some(None);
    }
    let addrs = read(std::mem::offset_of!(RtMsghdr, rtm_addrs));
    Some(
        sockaddr_ipv4(route_sockaddr(&message[header..msglen], addrs, RTA_IFA))
            .filter(|ip| !ip.is_unspecified() && !ip.is_loopback()),
    )
}

/// The IPv4 address in a sockaddr whose family is `AF_INET` (a `sockaddr_in`, or the ARP table's
/// `sockaddr_inarp`, which shares its first eight bytes): the four bytes at offset four. `None`
/// for another family or a sockaddr too short to hold it.
#[cfg(target_os = "macos")]
fn sockaddr_ipv4(sockaddr: Option<&[u8]>) -> Option<Ipv4Addr> {
    let sockaddr = sockaddr?;
    if i32::from(*sockaddr.get(1)?) != libc::AF_INET {
        return None;
    }
    let octets: [u8; 4] = sockaddr.get(4..8)?.try_into().ok()?;
    Some(Ipv4Addr::from(octets))
}

/// Whether a route message's destination sockaddr is the default route's (`0.0.0.0`): its family
/// is `AF_INET` or a length-only zero, and its four address bytes (a missing one reads zero) are
/// all zero. A host or subnet route names some other destination.
#[cfg(target_os = "macos")]
fn is_default_destination(dst: Option<&[u8]>) -> bool {
    let Some(dst) = dst else { return false };
    let family = i32::from(dst.get(1).copied().unwrap_or(0));
    if family != libc::AF_INET && family != 0 {
        return false;
    }
    (0..4).all(|i| dst.get(4 + i).copied().unwrap_or(0) == 0)
}

/// The hardware address in a link-layer `sockaddr_dl` (family `AF_LINK`): `sdl_alen` bytes at
/// `sdl_data` (offset eight) past the `sdl_nlen`-byte interface name. `None` for another family
/// or an empty address (an incomplete ARP entry).
#[cfg(target_os = "macos")]
fn sockaddr_dl_hardware(sockaddr: Option<&[u8]>) -> Option<Box<[u8]>> {
    let sockaddr = sockaddr?;
    if i32::from(*sockaddr.get(1)?) != libc::AF_LINK {
        return None;
    }
    let nlen = usize::from(*sockaddr.get(5)?);
    let alen = usize::from(*sockaddr.get(6)?);
    if alen == 0 {
        return None;
    }
    let start = 8 + nlen;
    Some(Box::from(sockaddr.get(start..start + alen)?))
}

/// The hardware address recorded for `gateway` in `/proc/net/arp`: the `HW address` column of the
/// row whose `IP address` and `Device` match, when it is a real address (not all zeroes, which is an
/// incomplete entry). `None` where no row names it. Rows without `ATF_COM` (`0x2`) are skipped so
/// a stale or incomplete entry on an inactive interface never shadows a complete entry below it.
#[cfg(any(test, target_os = "linux"))]
fn linux_neighbour_hardware(arp: &str, gateway: Ipv4Addr, interface: &str) -> Option<Box<[u8]>> {
    const ATF_COM: u32 = 0x2;
    for line in arp.lines().skip(1) {
        let mut column = line.split_whitespace();
        // Columns: IP address, HW type, Flags, HW address, Mask, Device.
        let (Some(ip), Some(_hw_type), Some(flags), Some(hardware)) =
            (column.next(), column.next(), column.next(), column.next())
        else {
            continue;
        };
        if column.nth(1) != Some(interface) {
            continue;
        }
        let Ok(ip) = ip.parse::<Ipv4Addr>() else {
            continue;
        };
        if ip != gateway {
            continue;
        }
        let flags_hex = flags
            .strip_prefix("0x")
            .or_else(|| flags.strip_prefix("0X"))
            .unwrap_or(flags);
        if flags_hex.is_empty() || !flags_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let Ok(arp_flags) = u32::from_str_radix(flags_hex, 16) else {
            continue;
        };
        if arp_flags & ATF_COM == 0 {
            continue;
        }
        let Some(bytes): Option<Vec<u8>> = hardware
            .split(':')
            .map(|byte| {
                (byte.len() == 2 && byte.bytes().all(|b| b.is_ascii_hexdigit()))
                    .then(|| u8::from_str_radix(byte, 16).ok())
                    .flatten()
            })
            .collect()
        else {
            continue;
        };
        if !bytes.is_empty() && !bytes.iter().all(|&byte| byte == 0) {
            return Some(bytes.into_boxed_slice());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_interfaces_are_stable_and_keep_attachment_changes() {
        // SAFETY: zero is a valid initial value for these sockaddr records.
        let mut v4: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        let mut v6: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
        v4.sin_family = libc::AF_INET as _;
        v4.sin_addr.s_addr = u32::from_ne_bytes([192, 0, 2, 7]);
        v6.sin6_family = libc::AF_INET6 as _;
        v6.sin6_addr.s6_addr = "2001:db8::7".parse::<Ipv6Addr>().unwrap().octets();
        #[cfg(target_os = "macos")]
        {
            v4.sin_len = std::mem::size_of_val(&v4) as u8;
            v6.sin6_len = std::mem::size_of_val(&v6) as u8;
        }
        struct Node {
            value: libc::ifaddrs,
            _next: Option<Box<Node>>,
        }
        let snapshot = |reverse: bool, running: bool| {
            let up = libc::IFF_UP as u32;
            let mut entries = vec![
                (false, up),
                (
                    true,
                    up | if running { libc::IFF_RUNNING as u32 } else { 0 },
                ),
                (false, up),
                (false, 0),
                (false, up | libc::IFF_LOOPBACK as u32),
            ];
            if reverse {
                entries.reverse();
            }
            let mut list: Option<Box<Node>> = None;
            for (ipv6, flags) in entries {
                // SAFETY: zero is a valid initial value for this C record.
                let mut value: libc::ifaddrs = unsafe { std::mem::zeroed() };
                value.ifa_next = list.as_ref().map_or(std::ptr::null_mut(), |next| {
                    std::ptr::from_ref(&next.value).cast_mut()
                });
                value.ifa_name = if ipv6 { c"en0" } else { c"en1" }.as_ptr().cast_mut();
                value.ifa_flags = flags;
                value.ifa_addr = if ipv6 {
                    std::ptr::from_ref(&v6).cast_mut().cast()
                } else {
                    std::ptr::from_ref(&v4).cast_mut().cast()
                };
                list = Some(Box::new(Node { value, _next: list }));
            }
            // SAFETY: every node, name and family-appropriate address stays
            // live and immutable until the read finishes. Boxed links do not move.
            unsafe { collect(std::ptr::from_ref(&list.as_ref().unwrap().value)) }
        };
        let before = snapshot(false, false);
        assert_eq!(before.len(), 2);
        assert_eq!(before[0].name, "en0");
        assert_eq!(before[1].address, "192.0.2.7".parse::<IpAddr>().unwrap());
        assert_eq!(before, snapshot(true, false));
        assert_ne!(before, snapshot(false, true), "link state changes matter");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_short_darwin_netmask_never_reads_past_its_length() {
        let mask = [6u8, libc::AF_INET as u8, 0, 0, 255, 255];
        // SAFETY: this is a live sockaddr header and its six advertised bytes.
        let value = unsafe { sockaddr_ip(mask.as_ptr().cast(), libc::AF_INET) };
        assert_eq!(value, Some(Ipv4Addr::new(255, 255, 0, 0).into()));
    }
}

/// A socket the kernel writes to on each change to this machine's addresses and links (and, on
/// Linux, routes): `PF_ROUTE` on macOS, `NETLINK_ROUTE` joined to those groups on Linux. A retry
/// that waits on it starts at the change (another Wi-Fi, a VPN, a wake), not at its next look.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub struct RouteChanges(std::os::fd::OwnedFd);

/// The netlink groups [`RouteChanges`] joins (`linux/rtnetlink.h`): links, and IPv4 and IPv6
/// addresses and routes; never neighbours, whose cache churns with LAN traffic.
#[cfg(target_os = "linux")]
const RTMGRP_CHANGES: u32 = 0x1 | 0x10 | 0x40 | 0x100 | 0x400;

/// How long [`RouteChanges::wait`] gathers the rest of a burst once a change came: one change
/// (an interface coming up) is several messages within milliseconds.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const BURST: std::time::Duration = std::time::Duration::from_millis(100);

/// The `PF_ROUTE` message types (`net/route.h`) that name a change of network: `RTM_NEWADDR`,
/// `RTM_DELADDR`, `RTM_IFINFO` and `RTM_IFINFO2`.
#[cfg(target_os = "macos")]
const NETWORK_CHANGES: [u8; 4] = [0xc, 0xd, 0xe, 0x12];

/// Whether one message a [`RouteChanges`] socket read names a change of network. On macOS
/// `PF_ROUTE` carries every routing-table message, the ARP and neighbour entries that come and
/// go with LAN traffic and each lookup's miss included: only an address added or removed, or a
/// link's state, counts (`rtm_type`, the fourth byte of every header). On Linux the groups
/// joined carry only changes. A read that overflowed (`ENOBUFS`) lost messages, so it counts.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn names_a_change(message: &[u8]) -> bool {
    #[cfg(target_os = "macos")]
    {
        message
            .get(3)
            .is_some_and(|kind| NETWORK_CHANGES.contains(kind))
    }
    #[cfg(target_os = "linux")]
    {
        !message.is_empty()
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl RouteChanges {
    /// A socket that hears each change from now on; `None` where the kernel refuses one.
    pub fn open() -> Option<RouteChanges> {
        use std::os::fd::FromRawFd as _;
        #[cfg(target_os = "macos")]
        let (domain, kind, protocol) = (libc::PF_ROUTE, libc::SOCK_RAW, libc::AF_UNSPEC);
        #[cfg(target_os = "linux")]
        let (domain, kind, protocol) = (
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            libc::NETLINK_ROUTE,
        );
        let fd = crate::kept_out(|| {
            // SAFETY: socket(2) takes no pointer; a descriptor it returns is owned below.
            let raw = unsafe { libc::socket(domain, kind, protocol) };
            if raw < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: `raw` is the fresh descriptor socket(2) returned, owned by nothing else.
            let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
            // Before the mark, as F_SETFD replaces every descriptor flag.
            #[cfg(target_os = "macos")]
            // SAFETY: F_SETFD takes an integer flag on this live descriptor.
            if unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(fd)
        })
        .ok()?;
        #[cfg(target_os = "linux")]
        let raw = std::os::fd::AsRawFd::as_raw_fd(&fd);
        #[cfg(target_os = "linux")]
        {
            // SAFETY: an all-zero sockaddr_nl is a valid value of the plain C struct.
            let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
            address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
            address.nl_groups = RTMGRP_CHANGES;
            // SAFETY: `address` is a live sockaddr_nl and the length passed is its size.
            let bound = unsafe {
                libc::bind(
                    raw,
                    std::ptr::from_ref(&address).cast(),
                    std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
                )
            };
            if bound < 0 {
                return None;
            }
        }
        Some(RouteChanges(fd))
    }

    /// Wait at most `most` for a change of network ([`names_a_change`]), then read the rest of
    /// its burst: whether one came. Messages that name none are read and passed over. A socket
    /// that fails otherwise waits out `most` and says none came, so a retry that waits on it
    /// keeps its own pace.
    pub fn wait(&self, most: std::time::Duration) -> bool {
        let now = std::time::Instant::now();
        let deadline = now
            .checked_add(most)
            .unwrap_or_else(|| now + std::time::Duration::from_secs(365 * 24 * 3600));
        let wait_out = || {
            std::thread::sleep(deadline.saturating_duration_since(std::time::Instant::now()));
            false
        };
        loop {
            match self.readable(deadline) {
                Some(true) => {}
                Some(false) => return false,
                None => return wait_out(),
            }
            match self.drain() {
                Some(true) => {
                    let burst = std::time::Instant::now() + BURST;
                    while self.readable(burst) == Some(true) && self.drain().is_some() {}
                    return true;
                }
                Some(false) => {}
                None => return wait_out(),
            }
        }
    }

    /// Whether a message is waiting by `deadline`; `None` when poll(2) failed otherwise than
    /// by a signal's interruption (which looks again).
    fn readable(&self, deadline: std::time::Instant) -> Option<bool> {
        use std::os::fd::AsRawFd as _;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let mut poll = libc::pollfd {
                fd: self.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = libc::c_int::try_from(left.as_millis()).unwrap_or(libc::c_int::MAX);
            // SAFETY: `poll` is one live pollfd and the count passed is 1.
            let ready = unsafe { libc::poll(&raw mut poll, 1, millis) };
            if ready >= 0 {
                return Some(ready > 0);
            }
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return None;
            }
        }
    }

    /// Read every message waiting: whether one named a change of network; `None` when the
    /// socket failed (an overflow, which lost messages, counts as a change).
    fn drain(&self) -> Option<bool> {
        use std::os::fd::AsRawFd as _;
        let mut buf = [0u8; 8192];
        let mut changed = false;
        loop {
            // SAFETY: `buf` is live and writable for its whole length.
            let read = unsafe {
                libc::recv(
                    self.0.as_raw_fd(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            if let Ok(read) = usize::try_from(read)
                && read > 0
            {
                changed |= names_a_change(&buf[..read]);
                continue;
            }
            let error = std::io::Error::last_os_error();
            return match error.raw_os_error() {
                _ if read == 0 => changed.then_some(true),
                Some(libc::EAGAIN | libc::EINTR) => Some(changed),
                Some(libc::ENOBUFS) => Some(true),
                _ => changed.then_some(true),
            };
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod route_change_tests {
    use super::*;

    /// The kernel gives a process its route-change socket, and a wait on it ends by its bound
    /// whether or not a change came meanwhile.
    #[test]
    fn a_route_change_socket_opens_and_its_wait_ends() {
        let changes = RouteChanges::open().expect("the kernel's route socket");
        let _came = changes.wait(std::time::Duration::from_millis(50));
    }

    /// An address added or removed, or a link's state, is a change of network; the ARP and
    /// neighbour entries LAN traffic adds and drops, and a lookup's miss, are not (macOS's
    /// `PF_ROUTE` carries them all).
    #[cfg(target_os = "macos")]
    #[test]
    fn only_an_address_or_a_link_is_a_change_of_network() {
        let message = |kind: u8| {
            let mut header = vec![0u8; 92];
            header[2] = 5; // RTM_VERSION
            header[3] = kind;
            header
        };
        // RTM_NEWADDR, RTM_DELADDR, RTM_IFINFO, RTM_IFINFO2.
        for kind in [0xc, 0xd, 0xe, 0x12] {
            assert!(names_a_change(&message(kind)), "type {kind:#x}");
        }
        // RTM_ADD, RTM_DELETE, RTM_CHANGE, RTM_GET, RTM_MISS, RTM_RESOLVE, RTM_NEWMADDR.
        for kind in [0x1, 0x2, 0x3, 0x4, 0x7, 0xb, 0xf] {
            assert!(!names_a_change(&message(kind)), "type {kind:#x}");
        }
        assert!(!names_a_change(&[]));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod gateway_tests {
    use super::*;

    /// A sockaddr_in carrying `ip`, as a `PF_ROUTE` message stores it.
    fn sockaddr_in(ip: Ipv4Addr) -> Vec<u8> {
        let mut sockaddr = vec![0u8; 16];
        sockaddr[0] = 16;
        sockaddr[1] = libc::AF_INET as u8;
        sockaddr[4..8].copy_from_slice(&ip.octets());
        sockaddr
    }

    /// A link-layer sockaddr_dl carrying `mac` and no interface name.
    fn sockaddr_dl(mac: &[u8]) -> Vec<u8> {
        let mut sockaddr = vec![0u8; 8 + mac.len()];
        sockaddr[0] = sockaddr.len() as u8;
        sockaddr[1] = libc::AF_LINK as u8;
        sockaddr[6] = mac.len() as u8; // sdl_alen; sdl_nlen stays zero
        sockaddr[8..].copy_from_slice(mac);
        sockaddr
    }

    /// One route message: `flags`, the present-sockaddr bitmask, and the sockaddrs each padded
    /// up to four bytes the way the kernel lays them out after the `rt_msghdr`.
    fn route_message(flags: i32, addrs: i32, sockaddrs: &[&[u8]]) -> Vec<u8> {
        let header = std::mem::size_of::<RtMsghdr>();
        let mut body = Vec::new();
        for sockaddr in sockaddrs {
            let sa_len = sockaddr.first().copied().unwrap_or(0);
            let advance = if sa_len == 0 {
                4
            } else {
                (usize::from(sa_len) + 3) & !3
            };
            let mut chunk = sockaddr.to_vec();
            chunk.resize(advance, 0);
            body.extend_from_slice(&chunk);
        }
        let mut message = vec![0u8; header];
        let msglen = (header + body.len()) as u16;
        message[0..2].copy_from_slice(&msglen.to_ne_bytes());
        let flags_at = std::mem::offset_of!(RtMsghdr, rtm_flags);
        let addrs_at = std::mem::offset_of!(RtMsghdr, rtm_addrs);
        message[flags_at..flags_at + 4].copy_from_slice(&flags.to_ne_bytes());
        message[addrs_at..addrs_at + 4].copy_from_slice(&addrs.to_ne_bytes());
        message.extend_from_slice(&body);
        message
    }

    #[test]
    fn the_default_route_names_the_gateway_and_a_host_route_does_not() {
        let gw = Ipv4Addr::new(192, 168, 1, 1);
        let default = route_message(
            RTF_UP | RTF_GATEWAY,
            RTA_DST | RTA_GATEWAY,
            &[&sockaddr_in(Ipv4Addr::UNSPECIFIED), &sockaddr_in(gw)],
        );
        let read = |table: &[u8]| {
            default_routes_of(table, &[], |_| Some("en0".into()))
                .primary
                .and_then(|route| route.gateway)
        };
        assert_eq!(read(&default), Some(gw));
        // Negative control: a host route to a non-default destination is not the default route,
        // so its gateway is never taken for the default gateway.
        let host = route_message(
            RTF_UP | RTF_GATEWAY,
            RTA_DST | RTA_GATEWAY,
            &[
                &sockaddr_in(Ipv4Addr::new(10, 0, 0, 5)),
                &sockaddr_in(Ipv4Addr::new(10, 0, 0, 1)),
            ],
        );
        assert_eq!(read(&host), None);
        // A table that leads with the host route still finds the default behind it.
        let mut both = host.clone();
        both.extend_from_slice(&default);
        assert_eq!(read(&both), Some(gw));
    }

    /// `message` with its interface index (`rtm_index`) set to `index`.
    fn on_interface(mut message: Vec<u8>, index: u16) -> Vec<u8> {
        let at = std::mem::offset_of!(RtMsghdr, rtm_index);
        message[at..at + 2].copy_from_slice(&index.to_ne_bytes());
        message
    }

    /// With Wi-Fi and Ethernet both attached, the kernel keeps an interface-scoped default on
    /// each and one primary: the primary alone names where traffic goes, whichever comes first
    /// in the table, and each route's gateway is paired with its own interface and with that
    /// interface's ARP entry, even where both gateways share an address.
    /// NEGATIVE CONTROL: dropping the `RTF_IFSCOPE` test makes the scoped Ethernet default,
    /// first in the table, the primary (`interface` reads `en7`), and this test fails.
    #[test]
    fn the_primary_default_is_the_unscoped_one_with_its_own_gateway_and_arp_entry() {
        let gw = Ipv4Addr::new(192, 168, 1, 1);
        let wifi_mac = [0xaa, 0xbb, 0xcc, 0x11, 0x22, 0x33];
        let wired_mac = [0xde, 0xad, 0xbe, 0xef, 0x00, 0x07];
        let default_on = |index: u16, flags: i32| {
            on_interface(
                route_message(
                    flags,
                    RTA_DST | RTA_GATEWAY,
                    &[&sockaddr_in(Ipv4Addr::UNSPECIFIED), &sockaddr_in(gw)],
                ),
                index,
            )
        };
        let mut table = default_on(7, RTF_UP | RTF_GATEWAY | RTF_IFSCOPE);
        table.extend_from_slice(&default_on(4, RTF_UP | RTF_GATEWAY));
        table.extend_from_slice(&default_on(4, RTF_UP | RTF_GATEWAY | RTF_IFSCOPE));
        let arp_on = |index: u16, mac: &[u8]| {
            on_interface(
                route_message(
                    0,
                    RTA_DST | RTA_GATEWAY,
                    &[&sockaddr_in(gw), &sockaddr_dl(mac)],
                ),
                index,
            )
        };
        let mut arp = arp_on(7, &wired_mac);
        arp.extend_from_slice(&arp_on(4, &wifi_mac));
        let name = |index: u16| match index {
            4 => Some("en0".to_owned()),
            7 => Some("en7".to_owned()),
            _ => None,
        };
        let routes = default_routes_of(&table, &arp, name);
        // The home-LAN evidence reader must use this same primary/interface pairing.
        // The former reader chose the first scoped route and the first matching ARP entry.
        assert_eq!(
            primary_gateway_hardware(&table, &arp, name).as_deref(),
            Some(wifi_mac.as_slice())
        );
        assert_eq!(
            primary_gateway_hardware(&table, &arp_on(7, &wired_mac), name),
            None
        );
        let primary = routes.primary.expect("the unscoped default");
        assert_eq!(primary.interface, "en0");
        assert_eq!(primary.gateway, Some(gw));
        assert_eq!(primary.gateway_hw.as_deref(), Some(wifi_mac.as_slice()));
        let scoped: Vec<(&str, Option<&[u8]>)> = routes
            .scoped
            .iter()
            .map(|route| (route.interface.as_str(), route.gateway_hw.as_deref()))
            .collect();
        assert_eq!(
            scoped,
            [
                ("en7", Some(wired_mac.as_slice())),
                ("en0", Some(wifi_mac.as_slice()))
            ]
        );
        // A default straight onto a link (a VPN's utun) has no IPv4 next hop.
        let utun = on_interface(
            route_message(
                RTF_UP,
                RTA_DST | RTA_GATEWAY,
                &[&sockaddr_in(Ipv4Addr::UNSPECIFIED), &sockaddr_dl(&[])],
            ),
            9,
        );
        let routes = default_routes_of(&utun, &arp, |_| Some("utun4".to_owned()));
        let primary = routes.primary.expect("the tunnel's default");
        assert_eq!(
            (primary.interface.as_str(), primary.gateway),
            ("utun4", None)
        );
        assert_eq!(default_routes_of(&[], &arp, name), DefaultRoutes::default());
    }

    #[test]
    fn the_arp_table_gives_the_gateways_hardware_address_and_nothing_for_a_stranger() {
        let gw = Ipv4Addr::new(192, 168, 1, 1);
        let mac = [0xaa, 0xbb, 0xcc, 0x11, 0x22, 0x33];
        let entry = route_message(
            0,
            RTA_DST | RTA_GATEWAY,
            &[&sockaddr_in(gw), &sockaddr_dl(&mac)],
        );
        assert_eq!(
            gateway_hardware_on(&entry, gw, 0).as_deref(),
            Some(mac.as_slice())
        );
        // Negative control: no entry names this gateway yet, so the key cannot be formed.
        assert_eq!(
            gateway_hardware_on(&entry, Ipv4Addr::new(192, 168, 1, 254), 0),
            None
        );
        // An incomplete entry (no hardware address) is no entry.
        let incomplete = route_message(
            0,
            RTA_DST | RTA_GATEWAY,
            &[&sockaddr_in(gw), &sockaddr_dl(&[])],
        );
        assert_eq!(gateway_hardware_on(&incomplete, gw, 0), None);
    }

    /// `route_source_ipv4`'s lookup asks the kernel for the route to its destination with the
    /// interface's slot set, as `route -n get` does, and reads the route's address only from the
    /// answer to its own lookup: another process's, another sequence's, or a route change is
    /// passed over, and a lookup the kernel refused or a loopback address is no source.
    #[test]
    fn a_route_lookup_asks_for_the_interface_and_reads_only_its_own_answer() {
        let to = Ipv4Addr::new(224, 0, 0, 251);
        let request = route_get_request(to, 7);
        let field = |message: &[u8], offset: usize| {
            i32::from_ne_bytes(message[offset..offset + 4].try_into().unwrap())
        };
        let header = std::mem::size_of::<RtMsghdr>();
        assert_eq!(
            usize::from(u16::from_ne_bytes([request[0], request[1]])),
            request.len()
        );
        assert_eq!(request[std::mem::offset_of!(RtMsghdr, rtm_version)], 5);
        assert_eq!(request[std::mem::offset_of!(RtMsghdr, rtm_type)], RTM_GET);
        let addrs = field(&request, std::mem::offset_of!(RtMsghdr, rtm_addrs));
        assert_eq!(
            addrs,
            RTA_DST | RTA_IFP,
            "the interface's slot asks for its address"
        );
        assert_eq!(field(&request, std::mem::offset_of!(RtMsghdr, rtm_seq)), 7);
        let block = &request[header..];
        assert_eq!(
            sockaddr_ipv4(route_sockaddr(block, addrs, RTA_DST)),
            Some(to)
        );
        let ifp = route_sockaddr(block, addrs, RTA_IFP).unwrap();
        assert_eq!(
            (usize::from(ifp[0]), i32::from(ifp[1])),
            (std::mem::size_of::<libc::sockaddr_dl>(), libc::AF_LINK)
        );

        let address = Ipv4Addr::new(192, 168, 1, 20);
        let answer = |kind: u8, pid: i32, sequence: i32, errno: i32, ifa: Option<Ipv4Addr>| {
            let netmask = [0u8; 0];
            let mut sockaddrs: Vec<Vec<u8>> = vec![
                sockaddr_in(to),
                sockaddr_dl(&[1, 0, 0x5e, 0, 0, 0xfb]),
                netmask.to_vec(),
                sockaddr_dl(&[]),
            ];
            let mut addrs = RTA_DST | RTA_GATEWAY | 0x4 | RTA_IFP;
            if let Some(ifa) = ifa {
                sockaddrs.push(sockaddr_in(ifa));
                addrs |= RTA_IFA;
            }
            let sockaddrs: Vec<&[u8]> = sockaddrs.iter().map(Vec::as_slice).collect();
            let mut message = route_message(RTF_UP | RTF_HOST, addrs, &sockaddrs);
            message[std::mem::offset_of!(RtMsghdr, rtm_type)] = kind;
            for (offset, value) in [
                (std::mem::offset_of!(RtMsghdr, rtm_pid), pid),
                (std::mem::offset_of!(RtMsghdr, rtm_seq), sequence),
                (std::mem::offset_of!(RtMsghdr, rtm_errno), errno),
            ] {
                message[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
            }
            message
        };
        assert_eq!(
            route_get_answer(&answer(RTM_GET, 41, 7, 0, Some(address)), 41, 7),
            Some(Some(address)),
            "its own answer names the route's address"
        );
        // Negative controls: another process's lookup, another of its own, and a route added
        // meanwhile are not its answer.
        assert_eq!(
            route_get_answer(&answer(RTM_GET, 42, 7, 0, Some(address)), 41, 7),
            None
        );
        assert_eq!(
            route_get_answer(&answer(RTM_GET, 41, 8, 0, Some(address)), 41, 7),
            None
        );
        assert_eq!(
            route_get_answer(&answer(0x1, 41, 7, 0, Some(address)), 41, 7),
            None
        );
        assert_eq!(
            route_get_answer(&request[..header - 1], 41, 7),
            None,
            "too short"
        );
        // Its own answer with no source in it.
        assert_eq!(
            route_get_answer(&answer(RTM_GET, 41, 7, libc::ESRCH, Some(address)), 41, 7),
            Some(None),
            "no route"
        );
        assert_eq!(
            route_get_answer(&answer(RTM_GET, 41, 7, 0, None), 41, 7),
            Some(None)
        );
        assert_eq!(
            route_get_answer(&answer(RTM_GET, 41, 7, 0, Some(Ipv4Addr::LOCALHOST)), 41, 7),
            Some(None)
        );
    }

    /// The kernel's own answer: the routing table gives the address a UDP socket connected to the
    /// same destination takes. The destination is TEST-NET-1 (`192.0.2.1`), which the default
    /// route carries off this Mac's network, so the connect asks macOS nothing; both read `None`
    /// on a Mac with no default route. Loopback's route is the exception: its connected socket
    /// takes `127.0.0.1`, and the lookup reads that address as none.
    #[test]
    fn the_route_lookup_gives_the_address_a_connected_socket_takes() {
        let connected = |to: Ipv4Addr| {
            std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
                .ok()
                .filter(|socket| socket.connect((to, 9)).is_ok())
                .and_then(|socket| socket.local_addr().ok())
                .and_then(|local| match local.ip() {
                    IpAddr::V4(ip) => Some(ip),
                    IpAddr::V6(_) => None,
                })
        };
        let to = Ipv4Addr::new(192, 0, 2, 1);
        assert_eq!(
            route_source_ipv4(to),
            connected(to).filter(|ip| !ip.is_unspecified() && !ip.is_loopback())
        );
        assert_eq!(connected(Ipv4Addr::LOCALHOST), Some(Ipv4Addr::LOCALHOST));
        assert_eq!(
            route_source_ipv4(Ipv4Addr::LOCALHOST),
            None,
            "loopback's route has no address but loopback's"
        );
    }
}

#[cfg(test)]
mod linux_gateway_tests {
    use super::*;

    #[test]
    fn proc_route_and_arp_name_the_default_gateways_hardware_address() {
        let route = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\n\
             tun0\t00000000\t0108000A\t0003\t0\t0\t50\t00000080\n\
             en0\t00000000\t00000000\t0001\t0\t0\t90\t00000000\n\
             en0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\n";
        assert_eq!(
            linux_default_routes(route, "").primary.unwrap().gateway,
            None,
            "the gatewayless primary must not borrow a later route's gateway"
        );
        // Negative control: with no default-destination row there is no gateway.
        let no_default = "Iface\tDestination\tGateway\tFlags\nen0\t0001A8C0\t00000000\t0001\n";
        assert_eq!(linux_default_routes(no_default, "").primary, None);
        let signed_route = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\n\
             en0\t00000000\t+101A8C0\t0003\t0\t0\t100\t00000000\n\
             en0\t00000000\t0101A8C0\t+003\t0\t0\t100\t00000000\n";
        assert_eq!(linux_default_routes(signed_route, "").primary, None);

        let arp = "IP address       HW type     Flags       HW address            Mask     Device\n\
             192.168.1.1      0x1         0x0         de:ad:be:ef:00:01     *        wlan0\n\
             192.168.1.1      0x1         0x+2        de:ad:be:ef:00:01     *        wlan0\n\
             192.168.1.1      0x1         0x2         +a:bb:cc:11:22:33     *        wlan0\n\
             192.168.1.1      0x1         0x2         aa:bb:cc:11:22:33     *        en0\n";
        assert_eq!(
            linux_neighbour_hardware(arp, Ipv4Addr::new(192, 168, 1, 1), "en0").as_deref(),
            Some([0xaa, 0xbb, 0xcc, 0x11, 0x22, 0x33].as_slice())
        );
        // Negative control: a stranger, and an incomplete entry, give nothing.
        assert_eq!(
            linux_neighbour_hardware(arp, Ipv4Addr::new(192, 168, 1, 9), "en0"),
            None
        );
        let incomplete = "IP address       HW type     Flags       HW address            Mask     Device\n\
             192.168.1.1      0x1         0x0         00:00:00:00:00:00     *        en0\n\
             192.168.1.1      0x1         0x0         de:ad:be:ef:00:01     *        en0\n";
        assert_eq!(
            linux_neighbour_hardware(incomplete, Ipv4Addr::new(192, 168, 1, 1), "en0"),
            None
        );
    }

    /// The first up `/0` row of `/proc/net/route` is the primary default, with its own
    /// interface, gateway and that gateway's ARP entry; a down row is passed over.
    #[test]
    fn proc_route_names_the_primary_defaults_interface_and_gateway() {
        let route = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\n\
             eth9\t00000000\t0101A8C0\t0002\t0\t0\t10\t00000000\n\
             wlan0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\n";
        let arp = "IP address       HW type     Flags       HW address            Mask     Device\n\
             192.168.1.1      0x1         0x2         de:ad:be:ef:00:07     *        eth9\n\
             192.168.1.1      0x1         0x2         aa:bb:cc:11:22:33     *        wlan0\n";
        let routes = linux_default_routes(route, arp);
        let primary = routes.primary.expect("a default");
        assert_eq!(primary.interface, "wlan0");
        assert_eq!(primary.gateway, Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(
            primary.gateway_hw.as_deref(),
            Some([0xaa, 0xbb, 0xcc, 0x11, 0x22, 0x33].as_slice())
        );
        assert!(routes.scoped.is_empty());
        // Negative control: no default row, no primary.
        let none = "Iface\tDestination\tGateway\tFlags\nen0\t0001A8C0\t00000000\t0001\n";
        assert_eq!(linux_default_routes(none, arp), DefaultRoutes::default());
    }
}
