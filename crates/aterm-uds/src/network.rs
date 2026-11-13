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
/// gateway, and the ARP/NDP table its hardware address (on macOS, where the table's dump leaves
/// the entry out, the kernel's answer to a lookup of it: [`default_routes`]). `None` where there
/// is no default route, where the gateway is not an IPv4 next hop, or where the gateway has no
/// neighbour entry yet. On macOS only the unscoped primary route counts, and its neighbour entry
/// must belong to that same interface, even when another attached network uses the same gateway
/// address.
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
            &route_table(NET_RT_FLAGS, RTF_LLINFO).unwrap_or_default(),
            interface_name,
            gateway_hardware_asked,
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
    /// That next hop's hardware address in the kernel's ARP entry for it on that same interface;
    /// `None` before ARP resolves it (after a wake or a join) or with no IPv4 next hop.
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
///
/// On macOS a gateway the ARP table's dump leaves out is looked up in the kernel instead
/// ([`gateway_hardware_asked`]): macOS 27.2 dumps a process outside Apple's own tools no
/// neighbour entry at all (`NET_RT_FLAGS`/`RTF_LLINFO` reads empty, `NET_RT_DUMP` holds no
/// `RTF_LLINFO` route, while `arp -an` lists them; measured 2026-11-05 on m9 and m113,
/// `docs/measured/e2e-emails-table-2026-11-05.md`), yet answers a routing socket's `RTM_GET` of
/// the gateway with its entry, as `route -n get` reads it.
#[must_use]
pub fn default_routes() -> DefaultRoutes {
    #[cfg(target_os = "macos")]
    {
        let Some(table) = route_table(NET_RT_DUMP, 0) else {
            return DefaultRoutes::default();
        };
        let arp = route_table(NET_RT_FLAGS, RTF_LLINFO).unwrap_or_default();
        default_routes_of(&table, &arp, interface_name, gateway_hardware_asked)
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
    ask: impl Fn(Ipv4Addr, u16) -> Option<Box<[u8]>>,
) -> Option<Box<[u8]>> {
    default_routes_of(table, arp, name_of, ask)
        .primary?
        .gateway_hw
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
/// each route's interface named by `name_of` from its index, and a gateway the `arp` table does
/// not name on its route's interface looked up by `ask` (the gateway and that interface's index:
/// [`gateway_hardware_asked`]). A route whose interface has no name is passed over: it cannot be
/// told apart from another.
#[cfg(target_os = "macos")]
fn default_routes_of(
    table: &[u8],
    arp: &[u8],
    name_of: impl Fn(u16) -> Option<String>,
    ask: impl Fn(Ipv4Addr, u16) -> Option<Box<[u8]>>,
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
        let gateway_hw = gateway.and_then(|gateway| {
            gateway_hardware_on(arp, gateway, index).or_else(|| ask(gateway, index))
        });
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
/// `RTM_VERSION` (`net/route.h`).
#[cfg(target_os = "macos")]
const RTM_VERSION: u8 = 5;
/// `RTM_GET` (`net/route.h`): a route lookup, and the type of the kernel's answer to one. The
/// kernel delivers the answer, and a lookup it refuses, to every routing socket on the Mac, the
/// asker's among them.
#[cfg(target_os = "macos")]
pub const RTM_GET: u8 = 0x4;
/// The route lookup the kernel answers to the asking socket alone: the answer is an ordinary
/// [`RTM_GET`] message carrying the asker's pid and sequence, and no other routing socket hears
/// anything. A destination no route takes is that answer's `rtm_errno` (`ESRCH`) on a write that
/// succeeds, where an [`RTM_GET`] of it fails the write and still delivers the refused message
/// to every routing socket.
///
/// The type is XNU-private: the public `net/route.h` leaves 0x11 out, and its name there
/// (`RTM_GET_SILENT`, a lookup the kernel keeps for its reachability callers) is recalled, not
/// read from an SDK. What is relied on was measured on macOS 27.0.1 (2026-11-12), from Apple's
/// own python and from an ad-hoc-signed Trust build, and is held by the live tests beside
/// [`route_get`]. A kernel that does not take the type refuses the write, and [`lookup_write`]
/// then asks with [`RTM_GET`].
#[cfg(target_os = "macos")]
pub const RTM_GET_SILENT: u8 = 0x11;
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

/// How long [`route_get`] waits for the kernel's answer to its lookup, which the kernel queues
/// before the write returns: a bound, never a pace.
#[cfg(all(target_os = "macos", not(test)))]
const ROUTE_ANSWER_WAIT: std::time::Duration = std::time::Duration::from_secs(1);
/// A unit test's is the one-minute hang detector (`aterm_spec::wait::MUST_HAPPEN`, which this
/// crate sits below): the answer always comes, but a loaded machine can keep the test's thread
/// from reading it within 1 s.
#[cfg(all(target_os = "macos", test))]
const ROUTE_ANSWER_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// The local IPv4 address this Mac's kernel sends from toward `to`: the address of the route
/// `to` takes, as `route -n get` reads it (a `PF_ROUTE` lookup that asks for the route's
/// interface, and so for its address, `RTA_IFA`). A UDP socket connected to `to` gets its
/// address from the same lookup, but on macOS that connect toward a local-network destination (a
/// LAN address, a multicast group such as mDNS's) can put macOS's Local Network question on
/// screen. A routing socket sends no packet and is no network socket, so it asks nothing. `None`
/// where no route takes `to`, the route has no IPv4 address but loopback's, or the kernel's
/// answer does not come ([`route_source_ipv4_asked`] tells the last from the others).
#[cfg(target_os = "macos")]
#[must_use]
pub fn route_source_ipv4(to: Ipv4Addr) -> Option<Ipv4Addr> {
    route_source_ipv4_asked(to).flatten()
}

/// [`route_source_ipv4`] with the ask that failed kept apart from the kernel's own "none":
/// `Some(found)` is the routing table's word (`Some(None)` where no route takes `to`, or its
/// route has no IPv4 address but loopback's), and `None` says the table was not read (no routing
/// socket, a write the kernel refused for anything but the missing route, no answer within
/// [`ROUTE_ANSWER_WAIT`]), so a caller with another way to ask takes it only then. Unlike a UDP
/// connect toward a destination no route takes, the ask written as [`RTM_GET_SILENT`] makes no
/// `RTM_MISS`, which the kernel sends to every routing socket on the Mac, and is heard by no
/// other socket; a process that has fallen back to [`RTM_GET`] ([`lookup_write`]) is heard by
/// every routing socket, as any `RTM_GET` is.
#[cfg(target_os = "macos")]
#[must_use]
pub fn route_source_ipv4_asked(to: Ipv4Addr) -> Option<Option<Ipv4Addr>> {
    table_word(route_get(to.into(), None, route_get_answer)).map(Option::flatten)
}

/// Whether a route takes the IPv6 destination `to`, asked of the routing table as
/// [`route_source_ipv4`] asks, with no scope: `Some(false)` where the kernel answers that none
/// does, and `None` where the table was not read. A UDP socket connected to a `to` no route
/// takes fails the same way, and that failure makes the kernel send an `RTM_MISS` to every
/// routing socket on the Mac; this ask, written as [`RTM_GET_SILENT`], is heard by nobody. In a
/// process that has fallen back to [`RTM_GET`] ([`lookup_write`]) it is heard by every routing
/// socket, as any `RTM_GET` is: its answer, or the refused request where no route takes `to`.
#[cfg(target_os = "macos")]
#[must_use]
pub fn routed_ipv6(to: Ipv6Addr) -> Option<bool> {
    table_word(route_get(to.into(), None, |message, pid, sequence| {
        Some(route_get_reply(message, pid, sequence)?.map(|_| ()))
    }))
    .map(|found| found.is_some())
}

/// What a lookup's outcome ([`route_get`]) says the routing table holds: `Some(Some(found))` for
/// a route, `Some(None)` where the kernel said no route takes the destination (`ESRCH`, as its
/// answer's `rtm_errno` or as its refusal of an [`RTM_GET`]'s write), `None` for any other
/// failure, which says nothing of the table.
#[cfg(target_os = "macos")]
fn table_word<T>(outcome: std::io::Result<T>) -> Option<Option<T>> {
    match outcome {
        Ok(found) => Some(Some(found)),
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Some(None),
        Err(_) => None,
    }
}

/// The hardware address of the kernel's ARP entry for `gateway` on the interface whose index is
/// `index`: the answer to a routing socket's lookup of `gateway` scoped to that interface
/// ([`route_get_request`]), read only where it is that interface's neighbour entry for that host
/// with a hardware address in it ([`route_get_link`]), as `route -n get -ifscope` reads it. It
/// sends no packet and resolves nothing: `None` before ARP resolves the gateway, where the
/// kernel answers with the interface's route instead, or where its answer does not come within
/// [`ROUTE_ANSWER_WAIT`]. [`default_routes`] asks it for a gateway the ARP table's dump leaves
/// out.
#[cfg(target_os = "macos")]
fn gateway_hardware_asked(gateway: Ipv4Addr, index: u16) -> Option<Box<[u8]>> {
    route_get(gateway.into(), Some(index), |message, pid, sequence| {
        route_get_link(message, pid, sequence, index)
    })
    .ok()
    .flatten()
}

/// Whether `error`, from the write of an [`RTM_GET_SILENT`] lookup, can be the kernel refusing
/// the message type: `EINVAL` (the socket hears none of its own messages, so the kernel has
/// nowhere to put a silent answer), `EPERM` (a kernel that knows no such type takes it for a
/// change to the table, which an unprivileged socket may not make) or `EOPNOTSUPP` (the same
/// kernel, asked by a privileged socket). The three are recalled from XNU's `route_output`, not
/// measured: macOS 27.0.1 takes the type. A destination no route takes (`ESRCH`) is the
/// kernel's answer to the lookup, never a refusal of its type.
#[cfg(target_os = "macos")]
fn silent_refused(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EINVAL | libc::EPERM | libc::EOPNOTSUPP)
    )
}

/// Write one route lookup, heard by its asker alone where the kernel allows: `write(kind)`
/// writes the lookup to a routing socket as message type `kind`, [`RTM_GET_SILENT`] first. Where
/// the kernel refuses that type ([`silent_refused`]) the same lookup is written once more as
/// [`RTM_GET`], and where the kernel takes that one, every later lookup of this process starts
/// there: one latch for the process, which aterm-net's lookups share by writing through here. A
/// lookup the kernel refuses in both types was refused for what it asks, so it latches nothing.
/// The answer to either type is an [`RTM_GET`] message with the asker's pid and sequence.
///
/// # Errors
///
/// The error of the last `write` made: the first where it failed for anything but the type, or
/// the process had already fallen back to [`RTM_GET`]; else the second.
#[cfg(target_os = "macos")]
pub fn lookup_write(write: impl FnMut(u8) -> std::io::Result<()>) -> std::io::Result<()> {
    static REFUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    lookup_write_latched(&REFUSED, write)
}

/// [`lookup_write`] over the latch `refused`, which a test supplies in place of the process's.
#[cfg(target_os = "macos")]
fn lookup_write_latched(
    refused: &std::sync::atomic::AtomicBool,
    mut write: impl FnMut(u8) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::sync::atomic::Ordering::Relaxed;
    if refused.load(Relaxed) {
        return write(RTM_GET);
    }
    match write(RTM_GET_SILENT) {
        Err(error) if silent_refused(&error) => {
            let plain = write(RTM_GET);
            if !plain.as_ref().is_err_and(silent_refused) {
                refused.store(true, Relaxed);
            }
            plain
        }
        written => written,
    }
}

/// A routing socket that hears the messages of address family `family` (0 for every family), as
/// each lookup opens one of its own.
#[cfg(target_os = "macos")]
fn routing_socket(family: libc::c_int) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd as _;
    crate::kept_out(|| {
        // SAFETY: socket(2) takes no pointer; a descriptor it returns is owned below.
        let raw = unsafe { libc::socket(libc::PF_ROUTE, libc::SOCK_RAW, family) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `raw` is the fresh descriptor socket(2) returned, owned by nothing else.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        // Before the mark, as F_SETFD replaces every descriptor flag.
        // SAFETY: F_SETFD takes an integer flag on this live descriptor.
        if unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(fd)
    })
}

/// The sequence number of this process's next lookup ([`route_get`]): no two share one, so an
/// answer names its lookup.
#[cfg(target_os = "macos")]
fn route_get_sequence() -> i32 {
    static SEQUENCE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
    SEQUENCE
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        .wrapping_add(1)
}

/// Write one lookup of `to` ([`route_get_request`], scoped to the interface whose index is `on`
/// where one is named) to a fresh routing socket, heard by this socket alone where the kernel
/// allows ([`lookup_write`]), and read the kernel's answer: what `read` makes of the first
/// message that answers this process's lookup (its pid and sequence).
///
/// # Errors
///
/// A destination no route takes is `ESRCH`: the answer's `rtm_errno`, or (asked as [`RTM_GET`])
/// the write's own failure, and any other `rtm_errno` comes back the same way. Otherwise the
/// routing socket's or the write's error, or `TimedOut` where no answer comes within
/// [`ROUTE_ANSWER_WAIT`].
#[cfg(target_os = "macos")]
fn route_get<T>(
    to: IpAddr,
    on: Option<u16>,
    read: impl Fn(&[u8], i32, i32) -> Option<Result<T, i32>>,
) -> std::io::Result<T> {
    route_get_as(
        to,
        on,
        route_get_sequence(),
        |write| lookup_write(write),
        read,
    )
}

/// [`route_get`] as lookup `sequence`, written the way `send` says: `send` is handed the writer
/// of this lookup in a message type of its choosing, which [`route_get`] gives to
/// [`lookup_write`] and a test calls with one type, or through a stand-in kernel's refusal.
#[cfg(target_os = "macos")]
fn route_get_as<T>(
    to: IpAddr,
    on: Option<u16>,
    sequence: i32,
    send: impl FnOnce(&mut dyn FnMut(u8) -> std::io::Result<()>) -> std::io::Result<()>,
    read: impl Fn(&[u8], i32, i32) -> Option<Result<T, i32>>,
) -> std::io::Result<T> {
    let pid = i32::try_from(std::process::id()).map_err(std::io::Error::other)?;
    // The socket hears its destination's family alone: an answer written as RTM_GET comes
    // through that filter, and other families' traffic never fills the room.
    let fd = routing_socket(match to {
        IpAddr::V4(_) => libc::AF_INET,
        IpAddr::V6(_) => libc::AF_INET6,
    })?;
    let raw = std::os::fd::AsRawFd::as_raw_fd(&fd);
    send(&mut |kind| {
        let request = route_get_request(to, sequence, on, kind);
        // SAFETY: `request` is live and readable for its whole length.
        let wrote = unsafe { libc::write(raw, request.as_ptr().cast(), request.len()) };
        match usize::try_from(wrote) {
            Ok(wrote) if wrote == request.len() => Ok(()),
            Ok(_) => Err(std::io::ErrorKind::WriteZero.into()),
            Err(_) => Err(std::io::Error::last_os_error()),
        }
    })?;
    let deadline = std::time::Instant::now() + ROUTE_ANSWER_WAIT;
    let mut buf = [0u8; 2048];
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        let mut poll = libc::pollfd {
            fd: raw,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = crate::deadline::poll_timeout_ms(left);
        // SAFETY: `poll` is one live pollfd and the count passed is 1.
        let ready = unsafe { libc::poll(&raw mut poll, 1, millis) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        if ready <= 0 {
            continue;
        }
        // SAFETY: `buf` is live and writable for its whole length.
        let got = unsafe { libc::recv(raw, buf.as_mut_ptr().cast(), buf.len(), 0) };
        let Ok(got) = usize::try_from(got) else {
            return Err(std::io::Error::last_os_error());
        };
        if let Some(answer) = read(&buf[..got], pid, sequence) {
            return answer.map_err(std::io::Error::from_raw_os_error);
        }
    }
}

/// The lookup [`route_get`] writes for `to` as `sequence`, in message type `kind`
/// ([`RTM_GET_SILENT`] or [`RTM_GET`]): the head, `to` as the destination (a `sockaddr_in` or a
/// `sockaddr_in6`), and an empty link-layer sockaddr in the interface's slot, which asks the
/// kernel to name the route's interface and its address in the answer, as `route -n get` asks.
/// With `on`, the lookup is scoped to the interface of that index (`RTF_IFSCOPE` and
/// `rtm_index`), as `route -n get -ifscope` asks: two networks whose gateways share an address
/// keep one entry each.
#[cfg(target_os = "macos")]
fn route_get_request(to: IpAddr, sequence: i32, on: Option<u16>, kind: u8) -> Vec<u8> {
    let header = std::mem::size_of::<RtMsghdr>();
    let mut destination = match to {
        IpAddr::V4(to) => {
            let mut destination = vec![0u8; std::mem::size_of::<libc::sockaddr_in>()];
            destination[1] = libc::AF_INET as u8;
            destination[4..8].copy_from_slice(&to.octets());
            destination
        }
        IpAddr::V6(to) => {
            let mut destination = vec![0u8; std::mem::size_of::<libc::sockaddr_in6>()];
            destination[1] = libc::AF_INET6 as u8;
            let at = std::mem::offset_of!(libc::sockaddr_in6, sin6_addr);
            destination[at..at + 16].copy_from_slice(&to.octets());
            destination
        }
    };
    destination[0] = destination.len() as u8;
    let mut interface = [0u8; std::mem::size_of::<libc::sockaddr_dl>()];
    interface[0] = interface.len() as u8;
    interface[1] = libc::AF_LINK as u8;
    let mut message = vec![0u8; header];
    let length = (header + destination.len() + interface.len()) as u16;
    message[0..2].copy_from_slice(&length.to_ne_bytes());
    message[std::mem::offset_of!(RtMsghdr, rtm_version)] = RTM_VERSION;
    message[std::mem::offset_of!(RtMsghdr, rtm_type)] = kind;
    let mut put = |offset: usize, value: i32| {
        message[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
    };
    let scope = if on.is_some() { RTF_IFSCOPE } else { 0 };
    put(
        std::mem::offset_of!(RtMsghdr, rtm_flags),
        RTF_UP | RTF_HOST | scope,
    );
    put(std::mem::offset_of!(RtMsghdr, rtm_addrs), RTA_DST | RTA_IFP);
    put(std::mem::offset_of!(RtMsghdr, rtm_seq), sequence);
    if let Some(index) = on {
        let at = std::mem::offset_of!(RtMsghdr, rtm_index);
        message[at..at + 2].copy_from_slice(&index.to_ne_bytes());
    }
    message.extend_from_slice(&destination);
    message.extend_from_slice(&interface);
    message
}

/// A route as the kernel's answer carries it: the answer's head (a [`RtMsghdr`]'s bytes) and its
/// address block.
#[cfg(target_os = "macos")]
type RouteFound<'a> = (&'a [u8], &'a [u8]);

/// The answer one message a routing socket read is to the lookup process `pid` wrote as
/// `sequence` ([`route_get_request`]): `None` when it answers something else (another process's
/// lookup, another of this one's, a change of a route), else `Some(Ok((head, block)))`, the
/// found route's head (a [`RtMsghdr`]'s bytes) and its address block, or `Some(Err(errno))`
/// where the kernel refused the lookup (`rtm_errno`: `ESRCH` where it found no route). The
/// answer to either type of lookup is an [`RTM_GET`] message.
#[cfg(target_os = "macos")]
fn route_get_reply(message: &[u8], pid: i32, sequence: i32) -> Option<Result<RouteFound<'_>, i32>> {
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
    let errno = read(std::mem::offset_of!(RtMsghdr, rtm_errno));
    if errno != 0 {
        return Some(Err(errno));
    }
    Some(Ok((&message[..header], &message[header..msglen])))
}

/// The `i32` field at `offset` of a route message's head.
#[cfg(target_os = "macos")]
fn head_i32(head: &[u8], offset: usize) -> i32 {
    i32::from_ne_bytes(head[offset..offset + 4].try_into().unwrap())
}

/// What one message a routing socket read says to [`route_source_ipv4`]'s lookup
/// ([`route_get_reply`]): `None` when it is not its answer, else `Ok` of the address of the
/// route found (`RTA_IFA`, `None` where that is no usable IPv4 one), or `Err(errno)` where the
/// kernel refused the lookup.
#[cfg(target_os = "macos")]
fn route_get_answer(
    message: &[u8],
    pid: i32,
    sequence: i32,
) -> Option<Result<Option<Ipv4Addr>, i32>> {
    Some(
        route_get_reply(message, pid, sequence)?.map(|(head, block)| {
            let addrs = head_i32(head, std::mem::offset_of!(RtMsghdr, rtm_addrs));
            sockaddr_ipv4(route_sockaddr(block, addrs, RTA_IFA))
                .filter(|ip| !ip.is_unspecified() && !ip.is_loopback())
        }),
    )
}

/// What one message a routing socket read says to [`gateway_hardware_asked`]'s lookup on the
/// interface whose index is `index` ([`route_get_reply`]): `None` when it is not its answer, else
/// `Ok` of the hardware address of the neighbour entry found, or `Err(errno)` where the kernel
/// refused the lookup. `Ok(None)` is anything found but that interface's ARP entry for one host
/// with an address in it: a route that is no host route (`RTF_HOST`) of the link layer
/// (`RTF_LLINFO`), as the interface's own route answers for a host ARP has not resolved; one on
/// another interface; an incomplete entry.
#[cfg(target_os = "macos")]
fn route_get_link(
    message: &[u8],
    pid: i32,
    sequence: i32,
    index: u16,
) -> Option<Result<Option<Box<[u8]>>, i32>> {
    Some(
        route_get_reply(message, pid, sequence)?.map(|(head, block)| {
            let flags = head_i32(head, std::mem::offset_of!(RtMsghdr, rtm_flags));
            let at = std::mem::offset_of!(RtMsghdr, rtm_index);
            let on = u16::from_ne_bytes([head[at], head[at + 1]]);
            let neighbour = flags & RTF_HOST != 0 && flags & RTF_LLINFO != 0 && on == index;
            let addrs = head_i32(head, std::mem::offset_of!(RtMsghdr, rtm_addrs));
            neighbour
                .then(|| sockaddr_dl_hardware(route_sockaddr(block, addrs, RTA_GATEWAY)))
                .flatten()
        }),
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
#[path = "network_tests.rs"]
mod tests;

/// A socket the kernel writes to on each change to this machine's addresses and links (and, on
/// Linux, routes): filtered `PF_SYSTEM` network events on macOS, `NETLINK_ROUTE` joined to
/// those groups on Linux. A retry
/// that waits on it starts at the change (another Wi-Fi, a VPN, a wake), not at its next look.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub struct RouteChanges(std::os::fd::OwnedFd);

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl std::os::fd::AsFd for RouteChanges {
    /// Borrow readiness so a network owner can poll route changes beside its own sockets. Its
    /// readiness is any message, a change or not, or an error: Linux reports a netlink overflow
    /// as POLLERR until a read takes it. Such an owner reads any readiness, an error included,
    /// with [`RouteChanges::read_waiting`] (an overflow reads as a change, and only a read that
    /// fails says `None`) and goes on waiting when that says no change came, as
    /// [`RouteChanges::wait_beside`] does for one other descriptor.
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(&self.0)
    }
}

/// What ended a [`RouteChanges::wait_beside`].
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteWake {
    /// A change of network came ([`names_a_change`], or an overflow), and the rest of its burst
    /// was read.
    Change,
    /// The other descriptor was readable, at its end or in error, before any change came. The
    /// caller reads it: nothing of it was consumed.
    Also,
    /// The deadline came with neither.
    Timeout,
    /// The route socket, or poll(2) itself, failed. A failed socket polls ready again at once,
    /// so the caller stops polling it rather than wait on it again.
    Failed,
}

/// The netlink groups [`RouteChanges`] joins (`linux/rtnetlink.h`): links, and IPv4 and IPv6
/// addresses and routes; never neighbours, whose cache churns with LAN traffic.
#[cfg(target_os = "linux")]
const RTMGRP_CHANGES: u32 = 0x1 | 0x10 | 0x40 | 0x100 | 0x400;

/// How long [`RouteChanges::wait`] gathers the rest of a burst once a change came: one change
/// (an interface coming up) is several messages within milliseconds.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const BURST: std::time::Duration = std::time::Duration::from_millis(100);

/// Bound one drain even if notifications arrive faster than this thread can consume them.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MAX_DRAIN_MESSAGES: usize = 256;

/// `sys/kern_event.h`: `_IOW('e', 2, struct kev_request)`, whose three fields are u32.
#[cfg(target_os = "macos")]
const SET_KERNEL_EVENT_FILTER: libc::c_ulong = 0x800c_6502;

/// `sys/sys_domain.h`'s SYSPROTO_EVENT (not present in the pinned libc surface).
#[cfg(target_os = "macos")]
const KERNEL_EVENT_PROTOCOL: libc::c_int = 1;

/// Whether a kernel notification names an address, router, attachment or link-state change.
/// macOS `net/net_kev.h` supplies the subclasses and event codes; the kernel filters the
/// vendor and class before delivery. Route lookups and misses never enter this socket.
/// On Linux every message of the groups joined is one but those that report no change
/// ([`netlink_names_a_change`]). An overflow (`ENOBUFS`) counts as a change.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn names_a_change(message: &[u8]) -> bool {
    #[cfg(target_os = "macos")]
    {
        let Some(header) = message.get(..24) else {
            return false;
        };
        let word =
            |at: usize| u32::from_ne_bytes(header[at..at + 4].try_into().expect("header word"));
        let size = usize::try_from(word(0)).unwrap_or(usize::MAX);
        if !(24..=message.len()).contains(&size) || word(4) != 1 || word(8) != 1 {
            return false;
        }
        match word(12) {
            // IPv4 address, subnet, destination, and router resolution changes.
            1 => matches!(word(20), 1..=7 | 9 | 10),
            // Link flags/media/attachment, protocol, address, capabilities and delegation.
            2 => matches!(word(20), 1..=6 | 9..=17 | 19 | 23 | 25 | 30),
            // IPv6 addresses and the default router; not requests for a NAT64 prefix.
            6 => matches!(word(20), 1..=6),
            _ => false,
        }
    }
    #[cfg(target_os = "linux")]
    {
        netlink_names_a_change(message)
    }
}

/// `linux/netlink.h` and `linux/rtnetlink.h`: a message header's length, the two message types
/// that can report no change, the fixed headers before their attributes, and the attributes
/// that tell (with `NLA_TYPE_MASK`, which drops the nested and byte-order flags).
#[cfg(target_os = "linux")]
mod netlink {
    pub(super) const HEADER: usize = 16;
    pub(super) const RTM_NEWLINK: u16 = 16;
    pub(super) const RTM_NEWADDR: u16 = 20;
    pub(super) const IFINFOMSG: usize = 16;
    pub(super) const IFADDRMSG: usize = 8;
    pub(super) const IFLA_WIRELESS: u16 = 11;
    pub(super) const IFA_CACHEINFO: u16 = 6;
    pub(super) const TYPE_MASK: u16 = 0x3fff;
}

/// Whether one datagram of the route socket names a change of network on Linux: any message in
/// it but two the kernel sends of a link or an address that stays as it was. A wireless
/// extensions event (`RTM_NEWLINK` carrying `IFLA_WIRELESS`) reports a scan's results or the
/// like (a radio that is up and joined to nothing sends one every few minutes); a link's own
/// change comes as its own message, which carries none. An address's lifetime refresh
/// (`RTM_NEWADDR` whose `IFA_CACHEINFO` update stamp has moved on from its creation stamp)
/// renews an address that was already there (each router advertisement renews every SLAAC
/// address it covers, a DHCP renewal its lease); an address added anew carries both stamps
/// equal. A datagram that does not parse counts as a change.
#[cfg(target_os = "linux")]
fn netlink_names_a_change(datagram: &[u8]) -> bool {
    use netlink::{
        HEADER, IFA_CACHEINFO, IFADDRMSG, IFINFOMSG, IFLA_WIRELESS, RTM_NEWADDR, RTM_NEWLINK,
    };
    let word = |bytes: &[u8], at: usize| -> Option<u32> {
        Some(u32::from_ne_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    let mut rest = datagram;
    while !rest.is_empty() {
        let Some(length) = word(rest, 0).and_then(|length| usize::try_from(length).ok()) else {
            return true;
        };
        let Some(message) = rest.get(..length).filter(|_| length >= HEADER) else {
            return true;
        };
        let kind = u16::from_ne_bytes([message[4], message[5]]);
        let unchanged = match kind {
            RTM_NEWLINK => {
                netlink_attribute(&message[HEADER..], IFINFOMSG, IFLA_WIRELESS).is_some()
            }
            RTM_NEWADDR => netlink_attribute(&message[HEADER..], IFADDRMSG, IFA_CACHEINFO)
                .and_then(|info| Some((word(info, 8)?, word(info, 12)?)))
                .is_some_and(|(created, updated)| created != updated),
            _ => false,
        };
        if !unchanged {
            return true;
        }
        rest = rest.get(length.next_multiple_of(4)..).unwrap_or_default();
    }
    false
}

/// The payload of attribute `wanted` among those after a message's fixed header of `fixed`
/// bytes, if it carries one; `None` where it does not or they do not parse.
#[cfg(target_os = "linux")]
fn netlink_attribute(payload: &[u8], fixed: usize, wanted: u16) -> Option<&[u8]> {
    let mut attributes = payload.get(fixed..)?;
    while let Some(head) = attributes.get(..4) {
        let length = usize::from(u16::from_ne_bytes([head[0], head[1]]));
        let kind = u16::from_ne_bytes([head[2], head[3]]) & netlink::TYPE_MASK;
        let body = attributes.get(4..length)?;
        if kind == wanted {
            return Some(body);
        }
        attributes = attributes.get(length.next_multiple_of(4)..)?;
    }
    None
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl RouteChanges {
    /// A socket that hears each change from now on.
    ///
    /// # Errors
    /// The errno of the step the kernel refused, so a caller that does without the socket can
    /// say why: `socket(2)` (`EMFILE` with no descriptor free), its close-on-exec flag or its
    /// event filter on macOS, `bind(2)` on Linux, or its mark ([`crate::kept_out`]).
    pub fn open() -> std::io::Result<RouteChanges> {
        use std::os::fd::FromRawFd as _;
        #[cfg(target_os = "macos")]
        let (domain, kind, protocol) = (libc::AF_SYSTEM, libc::SOCK_RAW, KERNEL_EVENT_PROTOCOL);
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
            #[cfg(target_os = "macos")]
            {
                // KEV_VENDOR_APPLE, KEV_NETWORK_CLASS, KEV_ANY_SUBCLASS. kern_event.h's
                // kev_request is exactly these three consecutive u32 fields.
                let filter = [1_u32, 1, 0];
                // SAFETY: ioctl reads the live twelve-byte filter in the declared C layout;
                // raw is this owned SYSPROTO_EVENT socket and the request is SIOCSKEVFILT.
                if unsafe { libc::ioctl(raw, SET_KERNEL_EVENT_FILTER, filter.as_ptr()) } < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(fd)
        })?;
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
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(RouteChanges(fd))
    }

    /// A stand-in whose messages a test writes in the kernel's format to `socket`'s peer.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn over(socket: std::os::fd::OwnedFd) -> RouteChanges {
        RouteChanges(socket)
    }

    /// Wait at most `most` for a change of network ([`names_a_change`]), then read the rest of
    /// its burst: whether one came. Messages that name none are read and passed over. A socket
    /// that fails otherwise waits out `most` and says none came, so a retry that waits on it
    /// keeps its own pace.
    pub fn wait(&self, most: std::time::Duration) -> bool {
        let deadline = Self::deadline(most);
        match self.until(deadline, None) {
            RouteWake::Change => true,
            RouteWake::Also | RouteWake::Timeout => false,
            RouteWake::Failed => {
                std::thread::sleep(deadline.saturating_duration_since(std::time::Instant::now()));
                false
            }
        }
    }

    /// Wait at most `most` for a change of network or for `also` (a descriptor the caller reads
    /// itself) to be ready, whichever comes first, and say which ended the wait. Messages that
    /// name no change are read and passed over and the wait goes on for the rest of its time, so
    /// a busy routing table or kernel event stream never ends it early. A zero `most` looks once.
    pub fn wait_beside(
        &self,
        most: std::time::Duration,
        also: Option<std::os::fd::BorrowedFd<'_>>,
    ) -> RouteWake {
        self.until(Self::deadline(most), also)
    }

    fn deadline(most: std::time::Duration) -> std::time::Instant {
        let now = std::time::Instant::now();
        now.checked_add(most)
            .unwrap_or_else(|| now + std::time::Duration::from_secs(365 * 24 * 3600))
    }

    /// [`Self::wait_beside`] to an absolute `deadline`.
    fn until(
        &self,
        deadline: std::time::Instant,
        also: Option<std::os::fd::BorrowedFd<'_>>,
    ) -> RouteWake {
        self.until_by(deadline, also, |fds, millis| {
            // SAFETY: `fds` is two live pollfds, each a descriptor borrowed for this call or -1,
            // which poll(2) passes over and reports nothing of.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, millis) };
            if ready < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        })
    }

    /// [`Self::until`] over `poll`, which fills in what poll(2) reports of the route socket and
    /// of `also` (or -1). Every report of a descriptor, an error or a hangup alone included, ends
    /// this pass's look at it: Linux reports a netlink overflow as POLLERR, and macOS a
    /// descriptor kqueue cannot watch as POLLNVAL, without POLLIN, and poll(2) reports either
    /// again at once until something takes it, so a report passed over would poll again at once
    /// to the deadline.
    fn until_by(
        &self,
        deadline: std::time::Instant,
        also: Option<std::os::fd::BorrowedFd<'_>>,
        mut poll: impl FnMut(&mut [libc::pollfd; 2], libc::c_int) -> std::io::Result<()>,
    ) -> RouteWake {
        use std::os::fd::AsRawFd as _;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let mut fds = [
                libc::pollfd {
                    fd: self.0.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: also.map_or(-1, |fd| fd.as_raw_fd()),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let millis = crate::deadline::poll_timeout_ms(left);
            if let Err(error) = poll(&mut fds, millis) {
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return RouteWake::Failed;
            }
            // The route socket first: what it holds is read now, a change or not, so a message
            // that names none never stays to end the next wait, and the read decides what an
            // error means (an overflow is a change; only a read that fails is a failure).
            if fds[0].revents != 0 {
                match self.read_waiting() {
                    Some(true) => return RouteWake::Change,
                    Some(false) => {}
                    None => return RouteWake::Failed,
                }
            }
            if fds[1].revents != 0 {
                return RouteWake::Also;
            }
            if left.is_zero() {
                return RouteWake::Timeout;
            }
        }
    }

    /// Read what a poll found waiting, and the rest of its burst when it named a change of
    /// network: whether one did. `None` when the socket failed, so a caller that polls it beside
    /// another descriptor waits out its own time rather than polling a failed socket again at once.
    pub fn read_waiting(&self) -> Option<bool> {
        let changed = self.drain()?;
        if changed {
            let burst = std::time::Instant::now() + BURST;
            while self.readable(burst) == Some(true) && self.drain().is_some() {}
        }
        Some(changed)
    }

    /// Whether a change of network came since the last read, asked without waiting: one bounded
    /// batch of what is queued now ([`MAX_DRAIN_MESSAGES`]), and no wait for the rest of a burst,
    /// which the next ask reads. For an owner that polls nothing and asks before it relies on
    /// something it read of the network earlier. A batch that did not reach the end of the queue
    /// says a change came, because a change may be among what it left unread. `None` when the
    /// socket failed: it tells nothing from then on, so the owner drops it.
    #[must_use]
    pub fn changed_now(&self) -> Option<bool> {
        Self::changed_now_by(|buf| self.receive_now(buf))
    }

    /// [`Self::changed_now`] over `recv`, one waiting message or the error that ends the batch.
    fn changed_now_by(mut recv: impl FnMut(&mut [u8]) -> std::io::Result<usize>) -> Option<bool> {
        let mut read = 0_usize;
        let changed = Self::drain_by(|buf| {
            let got = recv(buf);
            read += usize::from(matches!(got, Ok(1..)));
            got
        })?;
        Some(changed || read == MAX_DRAIN_MESSAGES)
    }

    /// Whether a message is waiting by `deadline`; `None` when poll(2) failed otherwise than
    /// by a signal's interruption (which looks again).
    fn readable(&self, deadline: std::time::Instant) -> Option<bool> {
        self.readable_by(deadline, std::time::Instant::now, |poll, millis| {
            // SAFETY: `poll` is one live pollfd, initialized by `readable_by`.
            let ready = unsafe { libc::poll(poll, 1, millis) };
            if ready < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(ready > 0)
            }
        })
    }

    fn readable_by(
        &self,
        deadline: std::time::Instant,
        now: impl Fn() -> std::time::Instant,
        mut poll: impl FnMut(&mut libc::pollfd, libc::c_int) -> std::io::Result<bool>,
    ) -> Option<bool> {
        use std::os::fd::AsRawFd as _;
        loop {
            let left = deadline.saturating_duration_since(now());
            if left.is_zero() {
                return Some(false);
            }
            let mut descriptor = libc::pollfd {
                fd: self.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = crate::deadline::poll_timeout_ms(left);
            match poll(&mut descriptor, millis) {
                Ok(ready) => return Some(ready),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return None,
            }
        }
    }

    /// Read a bounded batch of waiting messages: whether one named a change; `None` when the
    /// socket failed (an overflow, which lost messages, counts as a change).
    fn drain(&self) -> Option<bool> {
        Self::drain_by(|buf| self.receive_now(buf))
    }

    /// One waiting message into `buf`, never waiting for one: its length, or the errno
    /// (`EAGAIN` with nothing queued).
    fn receive_now(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::os::fd::AsRawFd as _;
        // SAFETY: `buf` is live and writable for its whole length. The socket stays
        // borrowed, and MSG_DONTWAIT keeps an empty receive from blocking.
        let read = unsafe {
            libc::recv(
                self.0.as_raw_fd(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                libc::MSG_DONTWAIT,
            )
        };
        usize::try_from(read).map_err(|_| std::io::Error::last_os_error())
    }

    fn drain_by(mut recv: impl FnMut(&mut [u8]) -> std::io::Result<usize>) -> Option<bool> {
        let mut buf = [0u8; 8192];
        let mut changed = false;
        for _ in 0..MAX_DRAIN_MESSAGES {
            match recv(&mut buf) {
                Ok(0) => return changed.then_some(true),
                Ok(read) => changed |= names_a_change(&buf[..read]),
                Err(error) => {
                    return match error.raw_os_error() {
                        Some(libc::EAGAIN | libc::EINTR) => Some(changed),
                        Some(libc::ENOBUFS) => Some(true),
                        _ => changed.then_some(true),
                    };
                }
            }
        }
        Some(changed)
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "network_drain_tests.rs"]
mod drain_tests;

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "network_route_change_tests.rs"]
mod route_change_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "network_gateway_tests.rs"]
mod gateway_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "network_route_lookup_tests.rs"]
mod route_lookup_tests;

#[cfg(test)]
#[path = "network_linux_gateway_tests.rs"]
mod linux_gateway_tests;
