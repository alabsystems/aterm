// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

    let mut near_cap = Kernel::new(900, 0);
    assert_eq!(
        read(&mut near_cap, 1024).map(|b| b.len()),
        Some(900),
        "headroom clamps to cap when need <= cap"
    );
    assert_eq!(near_cap.asks, [Some(64), None, Some(1024)]);

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
    let counts = tcp_summary_of_pcblist64(&buf)
        .map(|s| s.counts)
        .expect("valid pcblist64");
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
    assert_eq!(tcp_summary_of_pcblist64(without_trailer), None);

    // Bad xso_len is rejected.
    let mut bad_xso = xinpgen();
    bad_xso.extend_from_slice(&pcb(4, 99));
    bad_xso.extend_from_slice(&xinpgen());
    assert_eq!(tcp_summary_of_pcblist64(&bad_xso), None);
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
