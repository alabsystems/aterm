// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(unix)]
use std::path::Path;

use super::*;
#[cfg(unix)]
use crate::volumes::{Mount, Table};

#[test]
fn every_kernel_state_word_maps_to_one_run_state() {
    assert_eq!(RunState::from_bsd(1), RunState::Idle);
    assert_eq!(RunState::from_bsd(2), RunState::Running);
    assert_eq!(RunState::from_bsd(3), RunState::Sleeping);
    assert_eq!(RunState::from_bsd(4), RunState::Stopped);
    assert_eq!(RunState::from_bsd(5), RunState::Zombie);
    assert_eq!(RunState::from_bsd(9), RunState::Unknown);
    assert_eq!(RunState::from_mach_thread(1), RunState::Running);
    assert_eq!(RunState::from_mach_thread(2), RunState::Stopped);
    assert_eq!(RunState::from_mach_thread(3), RunState::Sleeping);
    assert_eq!(RunState::from_mach_thread(4), RunState::Uninterruptible);
    assert_eq!(RunState::from_mach_thread(5), RunState::Stopped);
    for (c, s) in [
        (b'R', RunState::Running),
        (b'S', RunState::Sleeping),
        (b'D', RunState::Uninterruptible),
        (b'T', RunState::Stopped),
        (b't', RunState::Stopped),
        (b'Z', RunState::Zombie),
        (b'I', RunState::Idle),
        (b'?', RunState::Unknown),
    ] {
        assert_eq!(RunState::from_linux(c), s, "{}", c as char);
    }
    assert_eq!(TcpState::from_darwin(4), TcpState::Established);
    assert_eq!(TcpState::from_darwin(2), TcpState::SynSent);
    assert_eq!(TcpState::from_linux(1), TcpState::Established);
    assert_eq!(TcpState::from_linux(0x0A), TcpState::Listen);
    assert_eq!(KnoteFilter::from_darwin(-1), KnoteFilter::Read);
    assert_eq!(KnoteFilter::from_darwin(-7), KnoteFilter::Timer);
    assert_eq!(KnoteFilter::from_darwin(-99), KnoteFilter::Other(-99));
}

fn put(b: &mut [u8], at: usize, v: &[u8]) {
    b[at..at + v.len()].copy_from_slice(v);
}

/// Device paths, and whether each names a terminal: both platforms' readers judge a device by
/// this one table (a macOS vnode's path, a Linux `fd/<n>` link).
const TERMINAL_PATHS: [(&str, bool); 12] = [
    ("/dev/ttys003", true),
    ("/dev/tty", true),
    ("/dev/tty.usbserial-10", true),
    ("/dev/cu.usbserial-10", true),
    ("/dev/ptyp0", true),
    ("/dev/ptmx", true),
    ("/dev/pts/3", true),
    ("/dev/console", true),
    ("/dev/null", false),
    ("/dev/zero", false),
    ("/dev/consoles", false),
    ("/dev/ptmx0", false),
];

#[test]
fn a_vnode_answer_gives_mode_size_identity_and_path() {
    let mut b = vec![0u8; darwin::VNODE_PATH_SIZE];
    put(
        &mut b,
        darwin::VNODE_OPENFLAGS_AT,
        &darwin::FWRITE.to_ne_bytes(),
    );
    put(
        &mut b,
        darwin::VNODE_MODE_AT,
        &(darwin::S_IFREG | 0o644).to_ne_bytes(),
    );
    put(&mut b, darwin::VNODE_DEV_AT, &16_777_232u32.to_ne_bytes());
    put(&mut b, darwin::VNODE_INO_AT, &987_654u64.to_ne_bytes());
    put(&mut b, darwin::VNODE_SIZE_AT, &36i64.to_ne_bytes());
    put(
        &mut b,
        darwin::VNODE_PATH_AT,
        b"/private/tmp/t/1.output\0junk",
    );
    let v = darwin::vnode(&b).unwrap();
    assert!(v.regular() && !v.char_device());
    assert_eq!((v.dev, v.ino, v.size), (16_777_232, 987_654, 36));
    assert_eq!(v.path, b"/private/tmp/t/1.output");
    put(&mut b, darwin::VNODE_SIZE_AT, &(-5i64).to_ne_bytes());
    assert_eq!(darwin::vnode(&b).unwrap().size, 0);
    assert_eq!(darwin::vnode(&b[..darwin::VNODE_PATH_SIZE - 1]), None);
    put(
        &mut b,
        darwin::VNODE_MODE_AT,
        &(darwin::S_IFCHR | 0o620).to_ne_bytes(),
    );
    let v = darwin::vnode(&b).unwrap();
    assert!(
        v.char_device() && !v.terminal(),
        "a device at a file's path"
    );
    for (path, terminal) in TERMINAL_PATHS {
        let mut at = path.as_bytes().to_vec();
        at.push(0);
        put(&mut b, darwin::VNODE_PATH_AT, &at);
        assert_eq!(darwin::vnode(&b).unwrap().terminal(), terminal, "{path}");
    }
    put(
        &mut b,
        darwin::VNODE_MODE_AT,
        &(darwin::S_IFREG | 0o644).to_ne_bytes(),
    );
    put(&mut b, darwin::VNODE_PATH_AT, b"/dev/ttys003\0");
    assert!(
        !darwin::vnode(&b).unwrap().terminal(),
        "a file is no terminal"
    );
}

#[test]
fn a_pipe_answer_gives_handles_flags_and_direction() {
    let mut b = [0u8; darwin::PIPE_SIZE];
    put(
        &mut b,
        darwin::PIPE_OPENFLAGS_AT,
        &darwin::FREAD.to_ne_bytes(),
    );
    put(&mut b, darwin::PIPE_QUEUED_AT, &65_536i64.to_ne_bytes());
    put(&mut b, darwin::PIPE_HANDLE_AT, &0xAAAAu64.to_ne_bytes());
    put(&mut b, darwin::PIPE_PEER_AT, &0xBBBBu64.to_ne_bytes());
    put(
        &mut b,
        darwin::PIPE_STATUS_AT,
        &darwin::PIPE_WANTW.to_ne_bytes(),
    );
    let p = darwin::pipe(&b).unwrap();
    assert_eq!((p.handle, p.peer, p.queued), (0xAAAA, 0xBBBB, 65_536));
    assert_eq!(p.status & darwin::PIPE_WANTW, darwin::PIPE_WANTW);
    assert!(!p.write_end());
    put(
        &mut b,
        darwin::PIPE_OPENFLAGS_AT,
        &darwin::FWRITE.to_ne_bytes(),
    );
    assert!(darwin::pipe(&b).unwrap().write_end());
    assert_eq!(darwin::pipe(&b[..10]), None);
}

fn tcp_bytes(state: i32, vflag: u8, faddr: &[u8], fport: u16, lport: u16, rcv: u32) -> Vec<u8> {
    let mut b = vec![0u8; darwin::SOCKET_SIZE];
    put(
        &mut b,
        darwin::SOCKET_KIND_AT,
        &darwin::SOCKINFO_TCP.to_ne_bytes(),
    );
    put(&mut b, darwin::SOCKET_TCP_STATE_AT, &state.to_ne_bytes());
    put(&mut b, darwin::SOCKET_VFLAG_AT, &[vflag]);
    put(&mut b, darwin::SOCKET_FADDR_AT, faddr);
    put(
        &mut b,
        darwin::SOCKET_FPORT_AT,
        &i32::from(fport.to_be()).to_ne_bytes(),
    );
    put(
        &mut b,
        darwin::SOCKET_LPORT_AT,
        &i32::from(lport.to_be()).to_ne_bytes(),
    );
    put(&mut b, darwin::SOCKET_RCV_CC_AT, &rcv.to_ne_bytes());
    b
}

#[test]
fn a_socket_answer_names_tcp_state_peer_and_queue() {
    let mut v4 = [0u8; 16];
    v4[12..].copy_from_slice(&[192, 0, 2, 10]);
    let b = tcp_bytes(4, darwin::INI_IPV4, &v4, 8080, 50_123, 0);
    let t = darwin::socket(&b).unwrap().unwrap();
    assert_eq!(TcpState::from_darwin(t.state), TcpState::Established);
    assert_eq!(t.remote, Some("192.0.2.10:8080".parse().unwrap()));
    assert_eq!((t.local_port, t.queued_read), (50_123, 0));

    let v6: [u8; 16] = "2001:db8::1"
        .parse::<std::net::Ipv6Addr>()
        .unwrap()
        .octets();
    let b = tcp_bytes(2, darwin::INI_IPV6, &v6, 443, 1, 7);
    let t = darwin::socket(&b).unwrap().unwrap();
    assert_eq!(t.remote, Some("[2001:db8::1]:443".parse().unwrap()));
    assert_eq!(t.queued_read, 7);

    let b = tcp_bytes(1, darwin::INI_IPV4, &[0; 16], 0, 5173, 0);
    assert_eq!(darwin::socket(&b).unwrap().unwrap().remote, None);

    let mut b = vec![0u8; darwin::SOCKET_SIZE];
    put(&mut b, darwin::SOCKET_KIND_AT, &3i32.to_ne_bytes());
    assert_eq!(darwin::socket(&b), Some(None));
    assert_eq!(darwin::socket(&b[..100]), None);
}

/// A `net.local.stream.pcblist_n` read: its `xinpgen` head, `records` each padded to 8, and its
/// `xinpgen` tail.
fn unix_table(records: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![0u8; 24];
    put(&mut b, 0, &24u32.to_ne_bytes());
    let tail = b.clone();
    for record in records {
        b.extend_from_slice(record);
        b.resize(b.len().next_multiple_of(8), 0);
    }
    b.extend_from_slice(&tail);
    b
}

/// A table record of `kind`, `len` bytes long.
fn table_record(kind: u32, len: usize) -> Vec<u8> {
    let mut b = vec![0u8; len];
    put(&mut b, 0, &u32::try_from(len).unwrap().to_ne_bytes());
    put(&mut b, 4, &kind.to_ne_bytes());
    b
}

/// One unix-domain stream's records, as the table lays them out: its `xunpcb_n` bound at `path`
/// (empty: unbound), its `xsocket_n` with `so_options` `options`, made by `uid` and last bound
/// by `pid`, and its receive buffer's record.
fn unix_stream(path: &[u8], options: u32, uid: u32, pid: u32) -> [Vec<u8>; 3] {
    let mut pcb = table_record(darwin::XSO_UNPCB, darwin::XUNPCB_SIZE);
    put(&mut pcb, darwin::XUNPCB_PATH_AT, path);
    let mut so = table_record(darwin::XSO_SOCKET, darwin::XSOCKET_SIZE);
    put(&mut so, darwin::XSOCKET_OPTIONS_AT, &options.to_ne_bytes());
    put(&mut so, darwin::XSOCKET_UID_AT, &uid.to_ne_bytes());
    put(&mut so, darwin::XSOCKET_LAST_PID_AT, &pid.to_ne_bytes());
    [pcb, so, table_record(0x2, 32)]
}

#[test]
fn the_unix_stream_table_names_each_bound_listener() {
    let path = b"/Users//u/f/bus.sock";
    let listens = u32::from(darwin::SO_ACCEPTCONN);
    let full = [b'p'; darwin::SUN_PATH_LEN];
    let mut no_nul = unix_stream(&full, listens, 0, 44);
    put(
        &mut no_nul[0],
        darwin::XUNPCB_PATH_AT + darwin::SUN_PATH_LEN,
        b"tail",
    );
    let table = unix_table(
        &[
            unix_stream(path, listens | 0x4, 501, 41),
            unix_stream(path, 0x4, 501, 42),
            unix_stream(b"", listens, 501, 43),
            no_nul,
        ]
        .concat(),
    );
    assert_eq!(
        darwin::unix_listeners(&table),
        Some(vec![
            darwin::UnixListenerRow {
                pid: 41,
                uid: 501,
                path: &path[..],
            },
            darwin::UnixListenerRow {
                pid: 44,
                uid: 0,
                path: &full[..],
            },
        ]),
        "a listener with another option set beside SO_ACCEPTCONN, and one whose path has no NUL \
         and ends at sun_path's length; not the stream it accepted, which carries its path, nor \
         an unbound listener"
    );
    assert_eq!(
        darwin::unix_listeners(&unix_table(&[])),
        Some(vec![]),
        "no streams"
    );
    assert_eq!(
        darwin::unix_listeners(&table[..table.len() - 30]),
        None,
        "a record running past the end"
    );
    for (kind, len) in [
        (darwin::XSO_UNPCB, darwin::XUNPCB_SIZE - 4),
        (darwin::XSO_SOCKET, darwin::XSOCKET_SIZE + 8),
    ] {
        let resized = unix_table(&[table_record(kind, len)]);
        assert_eq!(
            darwin::unix_listeners(&resized),
            None,
            "a record of kind {kind:#x} and another size"
        );
    }
    let mut zero = unix_table(&[table_record(0x2, 32)]);
    put(&mut zero, 24, &0u32.to_ne_bytes());
    assert_eq!(darwin::unix_listeners(&zero), None, "a length word under 8");
}

#[test]
fn a_kqueue_answer_gives_its_sleep_and_its_knotes() {
    let mut b = [0u8; darwin::KQUEUE_SIZE];
    put(
        &mut b,
        darwin::KQUEUE_STATE_AT,
        &(darwin::PROC_KQUEUE_SLEEP | 0x10).to_ne_bytes(),
    );
    assert_eq!(
        darwin::kqueue_state(&b).map(|s| s & darwin::PROC_KQUEUE_SLEEP),
        Some(darwin::PROC_KQUEUE_SLEEP)
    );
    let mut k = vec![0u8; darwin::KNOTE_SIZE * 2];
    put(&mut k, darwin::KNOTE_IDENT_AT, &3u64.to_ne_bytes());
    put(&mut k, darwin::KNOTE_FILTER_AT, &(-1i16).to_ne_bytes());
    put(
        &mut k,
        darwin::KNOTE_SIZE + darwin::KNOTE_IDENT_AT,
        &9u64.to_ne_bytes(),
    );
    put(
        &mut k,
        darwin::KNOTE_SIZE + darwin::KNOTE_FILTER_AT,
        &(-7i16).to_ne_bytes(),
    );
    assert_eq!(darwin::knotes(&k, 2), vec![(3, -1), (9, -7)]);
    assert_eq!(darwin::knotes(&k, 5).len(), 2);
}

#[test]
fn a_kinfo_answer_gives_a_zombies_parent_group_and_state() {
    let mut b = vec![0u8; darwin::KINFO_SIZE];
    put(
        &mut b,
        darwin::KINFO_START_SEC_AT,
        &1_760_000_000i64.to_ne_bytes(),
    );
    put(
        &mut b,
        darwin::KINFO_START_USEC_AT,
        &250_000i32.to_ne_bytes(),
    );
    put(
        &mut b,
        darwin::KINFO_FLAG_AT,
        &(darwin::P_CONTROLT | darwin::P_EXEC).to_ne_bytes(),
    );
    b[darwin::KINFO_STAT_AT] = 5;
    put(&mut b, darwin::KINFO_PID_AT, &6302u32.to_ne_bytes());
    put(&mut b, darwin::KINFO_COMM_AT, b"node\0");
    put(&mut b, darwin::KINFO_RUID_AT, &501u32.to_ne_bytes());
    put(&mut b, darwin::KINFO_PPID_AT, &6301u32.to_ne_bytes());
    put(&mut b, darwin::KINFO_PGID_AT, &6301u32.to_ne_bytes());
    put(&mut b, darwin::KINFO_TDEV_AT, &0x1000_0003u32.to_ne_bytes());
    put(&mut b, darwin::KINFO_TPGID_AT, &6301u32.to_ne_bytes());
    put(
        &mut b,
        darwin::KINFO_EFLAG_AT,
        &darwin::EPROC_SLEADER.to_ne_bytes(),
    );
    let k = darwin::kinfo(&b).unwrap();
    assert_eq!(
        (k.pid, k.ppid, k.pgid, k.stat, k.uid),
        (6302, 6301, 6301, 5, 501)
    );
    assert_eq!(k.start_us, 1_760_000_000_250_000);
    assert_eq!((k.tty, k.tpgid), (Some(0x1000_0003), Some(6301)));
    assert!(k.session_leader && k.exec);
    assert_eq!(k.comm, "node");
    put(&mut b, darwin::KINFO_FLAG_AT, &0u32.to_ne_bytes());
    let k = darwin::kinfo(&b).unwrap();
    assert_eq!((k.tty, k.exec), (None, false));
    assert_eq!(darwin::kinfo(&b[..100]), None);
}

#[test]
fn an_arch_answer_gives_its_cpu_type() {
    let mut b = vec![0u8; darwin::ARCHINFO_SIZE];
    put(
        &mut b,
        darwin::ARCHINFO_CPUTYPE_AT,
        &darwin::CPU_TYPE_X86_64.to_ne_bytes(),
    );
    assert_eq!(darwin::archinfo_cputype(&b), Some(darwin::CPU_TYPE_X86_64));
    assert_eq!(darwin::archinfo_cputype(&b[..4]), None);
}

/// M1's row reads held and M2's normal exec, caught at its first instruction, does not; each
/// other guard of [`judge_start`] moves M1's row off held.
#[test]
fn the_program_check_judge_holds_only_an_old_lone_unstarted_thread() {
    let m1 = StartFacts {
        lone_zero: true,
        stopped: false,
        traced: false,
        translated: false,
        local: Some(true),
        age_ms: Some(21_000),
    };
    let rows = [
        ("M1: one unstarted thread, 21 s", m1, AtStart::Held),
        (
            "M2: a normal exec at its first instruction",
            StartFacts {
                age_ms: Some(0),
                ..m1
            },
            AtStart::Started,
        ),
        (
            "exactly 20 s",
            StartFacts {
                age_ms: Some(PROGRAM_CHECK_AGE_MS),
                ..m1
            },
            AtStart::Held,
        ),
        (
            "19.999 s",
            StartFacts {
                age_ms: Some(19_999),
                ..m1
            },
            AtStart::Started,
        ),
        (
            "stopped",
            StartFacts {
                stopped: true,
                ..m1
            },
            AtStart::Started,
        ),
        (
            "traced",
            StartFacts { traced: true, ..m1 },
            AtStart::Started,
        ),
        (
            "a thread that ran",
            StartFacts {
                lone_zero: false,
                ..m1
            },
            AtStart::Started,
        ),
        (
            "x86_64 on arm64",
            StartFacts {
                translated: true,
                ..m1
            },
            AtStart::Translating,
        ),
        (
            "a remote volume",
            StartFacts {
                local: Some(false),
                ..m1
            },
            AtStart::Unstarted,
        ),
        (
            "an unread mount",
            StartFacts { local: None, ..m1 },
            AtStart::Unstarted,
        ),
        (
            "an unread age",
            StartFacts { age_ms: None, ..m1 },
            AtStart::Started,
        ),
    ];
    for (row, facts, want) in rows {
        assert_eq!(judge_start(&facts), want, "{row}: {facts:?}");
    }
}

const STAT: &str = "4711 (my (odd) cmd) S 4700 4711 4600 34817 4711 4194560 900 0 3 0 \
                    150 50 20 10 20 0 3 0 123456 0 0";

/// Every field is read past a command name holding parentheses. A terminal the kernel prints
/// negative (a pts minor of 2^19 or more sets its `int`'s sign bit) keeps its 32 bits rather than
/// reading as none, and one that fits neither an `int` nor a `u32` refuses the line.
#[test]
fn a_stat_line_gives_every_field_past_any_command_name() {
    let s = linux::parse_stat(STAT).unwrap();
    assert_eq!(s.name, "my (odd) cmd");
    assert_eq!(
        (s.state, s.ppid, s.pgrp, s.session),
        (b'S', 4700, 4711, 4600)
    );
    assert_eq!((s.tty_nr, s.tpgid), (34_817, 4711));
    assert_eq!(
        (s.majflt, s.utime, s.stime, s.cutime, s.cstime),
        (3, 150, 50, 20, 10)
    );
    assert_eq!((s.threads, s.start), (3, 123_456));
    assert!(s.exec());
    let fork = linux::parse_stat(&STAT.replacen(" 4194560 ", " 4194624 ", 1)).unwrap();
    assert!(!fork.exec(), "PF_FORKNOEXEC");
    let s = linux::parse_stat("9 (d) R 1 9 9 0 -1 0 0 0 0 0 1 2 0 0 20 0 1 0 7 0 0").unwrap();
    assert_eq!((s.tty_nr, s.tpgid), (0, 0));
    let far = STAT.replacen(" 34817 ", " -2147448832 ", 1);
    assert_eq!(
        linux::parse_stat(&far).map(|s| s.tty_nr),
        Some(0x8000_8800),
        "pts 524288 (major 136)"
    );
    for tty in ["x", "4294967296", "-2147483649"] {
        assert_eq!(
            linux::parse_stat(&STAT.replacen(" 34817 ", &format!(" {tty} "), 1)),
            None,
            "terminal {tty}"
        );
    }
    assert_eq!(linux::parse_stat("9 (d) R 1"), None);
    assert_eq!(linux::parse_stat("garbage"), None);
}

#[test]
fn io_status_schedstat_and_wchan_parse() {
    let io = linux::parse_io(
        "rchar: 10\nwchar: 2048\nsyscr: 5\nsyscw: 7\nread_bytes: 4096\nwrite_bytes: 8192\n\
         cancelled_write_bytes: 0\n",
    );
    assert_eq!(
        (io.wchar, io.syscr, io.syscw, io.read_bytes, io.write_bytes),
        (Some(2048), Some(5), Some(7), Some(4096), Some(8192))
    );
    let (uid, csw) = linux::parse_status(
        "Name:\tsleep\nUid:\t501\t501\t501\t501\nvoluntary_ctxt_switches:\t12\n\
         nonvoluntary_ctxt_switches:\t3\n",
    );
    assert_eq!((uid, csw), (Some(501), Some(15)));
    assert_eq!(linux::parse_schedstat("1500 300 4\n"), Some((1500, 300)));
    assert_eq!(
        linux::parse_wchan("hrtimer_nanosleep"),
        Some("hrtimer_nanosleep".to_owned())
    );
    assert_eq!(linux::parse_wchan("0"), None);
    assert_eq!(linux::parse_wchan(""), None);
    assert_eq!(
        linux::max_processes(
            "Limit                     Soft Limit           Hard Limit           Units\n\
             Max processes             63432                63432                processes\n"
        ),
        Some(63_432)
    );
    assert_eq!(
        linux::max_processes("Max processes             unlimited            unlimited\n"),
        None
    );
    assert!(linux::event_wait("ep_poll") && linux::event_wait("do_select"));
    assert!(!linux::event_wait("pipe_read"));
}

#[test]
fn fd_links_and_fdinfo_parse() {
    use linux::FdLink;
    assert_eq!(linux::parse_fd_link("pipe:[123]"), FdLink::Pipe(123));
    assert_eq!(linux::parse_fd_link("socket:[77]"), FdLink::Socket(77));
    assert_eq!(
        linux::parse_fd_link("anon_inode:[eventpoll]"),
        FdLink::EventPoll
    );
    assert_eq!(linux::parse_fd_link("anon_inode:inotify"), FdLink::Inotify);
    for (path, terminal) in TERMINAL_PATHS {
        let device = if terminal {
            FdLink::Tty
        } else {
            FdLink::Device
        };
        assert_eq!(linux::parse_fd_link(path), device, "{path}");
    }
    assert_eq!(
        linux::parse_fd_link("/tmp/out.log"),
        FdLink::Path("/tmp/out.log".into())
    );
    assert_eq!(linux::parse_fd_link("/proc/1/stat"), FdLink::Other);
    assert_eq!(linux::parse_fd_link("pipe:[x]"), FdLink::Other);
    let i = linux::parse_fdinfo("pos:\t36\nflags:\t02000001\nmnt_id:\t28\nino:\t991\n");
    assert_eq!((i.pos, i.ino), (36, Some(991)));
    assert!(i.write_only());
    let e = linux::parse_fdinfo(
        "pos:\t0\nflags:\t02\nmnt_id:\t15\ntfd:        5 events:       19 data: 5  pos:0 ino:4 sdev:d\n\
         tfd:        7 events:        4 data: 7  pos:0 ino:5 sdev:d\n",
    );
    assert_eq!(e.interests, vec![(5, 0x19), (7, 0x4)]);
    assert!(!e.write_only());
}

const NET_TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000   501        0 1111 1 0\n\
   1: 0100007F:C3A1 0A0200C0:1F90 01 00000000:00000000 00:00000000 00000000   501        0 2222 1 0\n\
   2: 0100007F:C3A2 0A0200C0:1F90 02 00000000:0000000";

/// A `/proc/net/unix` as Linux 6.8 prints one: a listener, a stream it accepted at the same path,
/// an abstract listener, an unbound socket, a listener whose path has a space, a listener whose
/// path is no UTF-8, and a torn row.
const NET_UNIX: &[u8] = b"\
Num       RefCount Protocol Flags    Type St Inode Path\n\
0000000000000000: 00000002 00000000 00010000 0001 01 41001 /run/user/1000/b.sock\n\
0000000000000000: 00000003 00000000 00000000 0001 03 41002 /run/user/1000/b.sock\n\
0000000000000000: 00000002 00000000 00010000 0001 01 41003 @/tmp/.X11-unix/X0\n\
0000000000000000: 00000002 00000000 00000000 0002 01  4104\n\
0000000000000000: 00000002 00000000 00010000 0001 01 41005 /tmp/a b.sock\n\
0000000000000000: 00000002 00000000 00010000 0001 01 41007 /tmp/\xff.sock\n\
0000000000000000: 00000002 00000000 00010000 0001 01 41006 /tmp/torn";

#[test]
fn listening_rows_of_proc_net_unix() {
    assert_eq!(
        linux::listening_unix(NET_UNIX),
        [
            (41001, &b"/run/user/1000/b.sock"[..]),
            (41005, b"/tmp/a b.sock"),
            (41007, b"/tmp/\xff.sock"),
        ],
        "listeners bound at a path, a stream accepted at that path left out"
    );
    assert!(linux::listening_unix(b"").is_empty());
    let header = NET_UNIX.split(|b| *b == b'\n').next().unwrap();
    assert!(linux::listening_unix(header).is_empty());
}

#[test]
fn a_tcp_table_gives_state_peer_and_queue_and_skips_a_torn_row() {
    let rows = linux::parse_net_tcp(NET_TCP, false);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].state, TcpState::Listen);
    assert_eq!((rows[0].local_port, rows[0].remote), (8080, None));
    assert_eq!(rows[1].state, TcpState::Established);
    assert_eq!(rows[1].remote, Some("192.0.2.10:8080".parse().unwrap()));
    assert_eq!((rows[1].inode, rows[1].rx_queue), (2222, 0));
    let v6 = "  sl  local remote st q\n\
   0: 00000000000000000000000001000000:1F90 B80D0120000000000000000001000000:01BB 01 00000000:00000005 00:0 0 501 0 3333 1\n";
    let rows = linux::parse_net_tcp(v6, true);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].remote, Some("[2001:db8::1]:443".parse().unwrap()));
    assert_eq!(rows[0].rx_queue, 5);
}

#[cfg(unix)]
fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

#[cfg(unix)]
fn link(root: &Path, rel: &str, target: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target, p).unwrap();
}

/// A `/proc` tree with one agent call: `4711` (`cat`) reads a pipe whose
/// writer is `4712`, holds an output file on fd 1, a TCP socket and an
/// epoll set; `4713` is a zombie; `4714` belongs to another user (no `io`).
#[cfg(unix)]
#[test]
fn the_linux_arm_reads_a_fixture_tree() {
    let dir = aterm_tempfile::tempdir().expect("scratch root");
    let r = dir.path();
    write(r, "4711/stat", STAT);
    write(
        r,
        "4711/status",
        "Uid:\t501\t501\t501\t501\nvoluntary_ctxt_switches:\t4\nnonvoluntary_ctxt_switches:\t1\n",
    );
    write(
        r,
        "4711/io",
        "wchar: 36\nsyscr: 9\nsyscw: 3\nread_bytes: 0\nwrite_bytes: 4096\n",
    );
    write(r, "4711/schedstat", "2000000000 500000000 10\n");
    write(r, "4711/wchan", "pipe_read");
    write(r, "4711/task/4711/stat", STAT);
    write(r, "4711/task/4711/wchan", "pipe_read");
    write(r, "4711/task/4715/stat", &STAT.replace(" S ", " D "));
    link(r, "4711/fd/0", "pipe:[555]");
    write(r, "4711/fdinfo/0", "pos:\t0\nflags:\t00\n");
    link(r, "4711/fd/1", "/tmp/claude/t/1.output");
    write(r, "4711/fdinfo/1", "pos:\t36\nflags:\t02001\nino:\t991\n");
    link(r, "4711/fd/2", "/dev/pts/3");
    link(r, "4711/fd/3", "socket:[2222]");
    link(r, "4711/fd/4", "anon_inode:[eventpoll]");
    write(
        r,
        "4711/fdinfo/4",
        "pos:\t0\nflags:\t02\ntfd:        3 events:       19 data: 3\n",
    );
    link(r, "4711/fd/5", "anon_inode:inotify");
    write(r, "4711/net/tcp", NET_TCP);
    write(
        r,
        "4713/stat",
        "4713 (zed) Z 4711 4711 4600 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 9 0 0",
    );
    write(
        r,
        "4714/stat",
        "4714 (other) S 1 4714 4714 0 -1 0 0 0 0 0 5 5 0 0 20 0 1 0 9 0 0",
    );

    let f = linux::read(r, 4711, true, 100).unwrap();
    assert_eq!(
        (f.ppid, f.pgid, f.sid, f.start),
        (4700, 4711, Some(4600), Some(123_456))
    );
    assert_eq!(
        (f.tty, f.tpgid, f.uid),
        (Some(34_817), Some(4711), Some(501))
    );
    assert_eq!(f.state, RunState::Sleeping);
    assert_eq!(f.wchan.as_deref(), Some("pipe_read"));
    let c = f.counters.unwrap();
    assert_eq!(c.cpu_ns, 2_000_000_000, "200 ticks at 100 Hz");
    assert_eq!(c.child_cpu_ns, Some(300_000_000));
    assert_eq!(
        (c.syscalls, c.ctx_switches, c.logical_writes),
        (Some(12), Some(5), Some(36))
    );
    assert_eq!(
        (c.disk_written, c.runnable_ns, c.pageins),
        (Some(4096), Some(2_500_000_000), Some(3))
    );
    let ts = f.threads.unwrap();
    assert_eq!(ts.len(), 2);
    assert_eq!(ts[1].state, RunState::Uninterruptible);
    assert_eq!(ts[0].wchan.as_deref(), Some("pipe_read"));
    let fds = f.fds.unwrap();
    let kind = |n: i32| fds.iter().find(|d| d.fd == n).map(|d| d.kind.clone());
    assert_eq!(
        kind(0),
        Some(FdKind::Pipe {
            handle: 555,
            peer: 555,
            status: None,
            queued: None,
            write_end: Some(false)
        })
    );
    assert_eq!(
        kind(1),
        Some(FdKind::File {
            regular: true,
            size: 36,
            dev: 0,
            ino: 991,
            lf_tail: None,
            fresh_dylib: false
        })
    );
    assert_eq!(kind(2), Some(FdKind::Device { tty: true }));
    assert_eq!(
        kind(3),
        Some(FdKind::Tcp {
            state: TcpState::Established,
            remote: Some("192.0.2.10:8080".parse().unwrap()),
            local_port: 50_081,
            queued_read: Some(0)
        })
    );
    assert_eq!(
        kind(4),
        Some(FdKind::EventQueue {
            sleeping: Some(false),
            knotes: Some(vec![Knote {
                ident: 3,
                filter: KnoteFilter::Read
            }])
        })
    );
    assert_eq!(kind(5), Some(FdKind::Watch));

    let z = linux::read(r, 4713, true, 100).unwrap();
    assert_eq!(z.state, RunState::Zombie);
    assert!(z.counters.is_none() && z.threads.is_none() && z.fds.is_none());
    let o = linux::read(r, 4714, false, 100).unwrap();
    assert!(o.counters.is_none());
    assert!(o.fds.is_none());
    assert!(linux::read(r, 9999, false, 100).is_none());
}

#[cfg(unix)]
fn table(mounts: &[(&str, bool, bool)]) -> Table {
    Table {
        mounts: mounts
            .iter()
            .map(|(on, local, automounted)| Mount {
                on: (*on).into(),
                from: "/dev/disk3s5".to_owned(),
                fstype: if *automounted { "autofs" } else { "apfs" }.to_owned(),
                local: *local,
                automounted: *automounted,
                removable: false,
                read_only: false,
                container: None,
            })
            .collect(),
        complete: true,
        firmlinks: Vec::new(),
    }
}

/// Only a temporary-files root on a local, non-automounted mount is opened: never a home folder,
/// a remote mount under `/tmp`, an automount or a `..` that climbs out. Unix only: [`TAIL_ROOTS`]
/// are unix folders, and off unix [`tail_has_lf`] answers `None`.
#[cfg(unix)]
#[test]
fn only_a_local_temporary_file_is_opened_for_its_tail() {
    let t = table(&[
        ("/", true, false),
        ("/private/tmp/nfs", false, false),
        ("/tmp/auto", true, true),
    ]);
    assert!(tail_allowed(
        Path::new("/private/tmp/agent-501/t/1.output"),
        &t
    ));
    assert!(tail_allowed(Path::new("/private/var/folders/x/T/out"), &t));
    assert!(!tail_allowed(Path::new("/Users//me/Documents/out.log"), &t));
    assert!(!tail_allowed(Path::new("/private/tmp/nfs/out.log"), &t));
    assert!(!tail_allowed(Path::new("/tmp/auto/out.log"), &t));
    assert!(!tail_allowed(Path::new("/private/tmp/../Users//me/x"), &t));
    assert!(!tail_allowed(Path::new("relative/tmp/x"), &t));
    assert!(!tail_allowed(Path::new("/private/tmp/x"), &table(&[])));
    let cut = Table {
        complete: false,
        ..t
    };
    assert!(
        !tail_allowed(Path::new("/private/tmp/agent-501/t/1.output"), &cut),
        "a cut table proves no mount local"
    );
}

/// A cut mount table proves no volume local (the mount it left out may hold the path), while a
/// mount it lists as remote still reads remote.
#[cfg(unix)]
#[test]
fn a_cut_mount_table_proves_no_volume_local() {
    let whole = table(&[("/", true, false), ("/Volumes/share", false, false)]);
    let cut = Table {
        complete: false,
        ..whole.clone()
    };
    let (gh, shared) = (Path::new("/usr/bin/gh"), Path::new("/Volumes/share/gh"));
    assert_eq!(local_at(gh, &whole), Some(true));
    assert_eq!(local_at(gh, &cut), None, "the cut may hold its mount");
    assert_eq!(local_at(shared, &whole), Some(false));
    assert_eq!(local_at(shared, &cut), Some(false));
    assert_eq!(local_at(Path::new("relative/gh"), &whole), None);
}

#[cfg(unix)]
#[test]
fn the_tail_check_reads_only_the_appended_bytes() {
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let p = dir.path().join("out");
    std::fs::write(&p, b"line one\nspin\rspin\r").unwrap();
    assert_eq!(tail_has_lf(&p, 0, 9), Some(true));
    assert_eq!(tail_has_lf(&p, 9, 19), Some(false));
    assert_eq!(tail_has_lf(&p, 9, 9), None);
    assert_eq!(tail_has_lf(&dir.path().join("missing"), 0, 4), None);
    let l = dir.path().join("link");
    std::os::unix::fs::symlink(&p, &l).unwrap();
    assert_eq!(tail_has_lf(&l, 0, 9), None);
}

#[cfg(target_os = "macos")]
#[test]
fn this_process_reads_itself_with_threads_counters_and_descriptors() {
    let mut fds = [0 as std::ffi::c_int; 2];
    // SAFETY: two writable descriptors' room.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let mut reader = JobsReader::new();
    let me = std::process::id();
    let f = reader
        .read(ReadReq { pid: me, fds: true })
        .expect("this process reads itself");
    assert!(f.ppid > 0 && f.pgid > 0, "{f:?}");
    assert!(f.start.is_some_and(|s| s > 0));
    assert!(f.sid.is_some());
    let own = std::env::current_exe().expect("this test's executable");
    assert_eq!(
        f.exe.as_deref(),
        own.file_name().and_then(|n| n.to_str()),
        "{f:?}"
    );
    assert!(
        f.counters
            .is_some_and(|c| c.cpu_ns > 0 && c.syscalls.is_some())
    );
    let ts = f.threads.as_ref().expect("threads");
    assert!(!ts.is_empty());
    assert!(
        ts.iter().any(|t| t.state == RunState::Running),
        "the reading thread is on a core: {ts:?}"
    );
    let list = f.fds.as_ref().expect("descriptors");
    let pipe = |n| {
        list.iter().find_map(|d| match d.kind {
            FdKind::Pipe {
                handle,
                peer,
                write_end,
                ..
            } if d.fd == n => Some((handle, peer, write_end)),
            _ => None,
        })
    };
    let (rh, rp, rw) = pipe(fds[0]).expect("the read end");
    let (wh, wp, ww) = pipe(fds[1]).expect("the write end");
    assert_eq!((rp, wp), (wh, rh), "each end's peer is the other's handle");
    assert_eq!((rw, ww), (Some(false), Some(true)));
    // SAFETY: our own descriptors.
    unsafe {
        libc::close(fds[0]);
        libc::close(fds[1]);
    }
}

/// A PROCESS'S SOCKETS ARE READ ONLY ONCE IT IS SETTLED (`macos::settled`): it ran this program at
/// the reader's last pass too, and it is not exiting. A fork that never exec'd, a program the last
/// pass did not see run, and one on its way out of `exit` are not.
#[cfg(target_os = "macos")]
#[test]
fn only_a_settled_process_has_its_sockets_read() {
    use darwin::{PROC_FLAG_EXEC as EXEC, PROC_FLAG_INEXIT as EXIT};
    assert!(macos::settled(EXEC, true));
    assert!(
        !macos::settled(EXEC, false),
        "a program the last pass did not see run"
    );
    assert!(!macos::settled(0, true), "a fork that never exec'd");
    assert!(!macos::settled(EXEC | EXIT, true), "on its way out of exit");
}

/// THE READER WAITS ONE PASS BEFORE IT READS A SOCKET: this process's socket reads `Other` at a
/// reader's first pass and in detail at the next, which finds the same program running. NEGATIVE
/// CONTROL: a fresh reader, with no last pass, reads it `Other` again.
#[cfg(target_os = "macos")]
#[test]
fn a_socket_reads_in_detail_from_the_second_pass_that_finds_its_program() {
    use std::os::fd::AsRawFd as _;
    let (end, _peer) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
    let fd = end.as_raw_fd();
    let me = std::process::id();
    let kind = |reader: &mut JobsReader| {
        reader
            .read(ReadReq { pid: me, fds: true })
            .and_then(|f| f.fds)
            .and_then(|fds| fds.into_iter().find(|d| d.fd == fd))
            .map(|d| d.kind)
    };
    let mut reader = JobsReader::new();
    assert_eq!(
        kind(&mut reader),
        Some(FdKind::Other),
        "the first pass reads no socket"
    );
    assert_eq!(
        kind(&mut reader),
        Some(FdKind::Socket),
        "the second reads it"
    );
    assert_eq!(kind(&mut JobsReader::new()), Some(FdKind::Other));
}

/// Both lanes pass their self-check in a plain test binary.
#[cfg(target_os = "macos")]
#[test]
fn both_lanes_self_check_on() {
    let lanes = JobsReader::new().lanes();
    assert_eq!(lanes.kqueue_detail, Lane::On, "{lanes:?}");
    assert_eq!(lanes.pipe_flags, Lane::On, "{lanes:?}");
}

/// A stopped child reads `Stopped`, a zombie child `Zombie` with no
/// counters, and the session leader of a new session leads it with no
/// terminal.
#[cfg(target_os = "macos")]
#[test]
fn a_stopped_child_and_a_zombie_child_read_their_states() {
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("60")
        .setsid(true)
        .spawn()
        .expect("sleep starts");
    let pid = child.id();
    let mut reader = JobsReader::new();
    let f = reader
        .read(ReadReq { pid, fds: true })
        .expect("the child reads");
    assert_eq!(f.sid, Some(pid), "it leads its own session");
    assert_eq!((f.tty, f.tpgid), (None, None));
    assert_eq!(f.ppid, std::process::id());
    let sweep = crate::processes().expect("a sweep");
    let row = sweep
        .procs
        .iter()
        .find(|p| p.pid == pid)
        .expect("the child is swept");
    assert_eq!(
        (row.session_leader, row.has_tty),
        (Some(true), Some(false)),
        "{row:?}"
    );
    // SAFETY: our own child.
    assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGSTOP) }, 0);
    let stopped = (0..6000).find_map(|_| {
        let s = reader.read(ReadReq { pid, fds: false }).map(|f| f.state);
        if s == Some(RunState::Stopped) {
            s
        } else {
            std::thread::sleep(std::time::Duration::from_millis(10));
            None
        }
    });
    assert_eq!(
        stopped,
        Some(RunState::Stopped),
        "hang detector: one minute"
    );
    child.kill().expect("kill");
    let zombie = (0..6000).find_map(|_| {
        let f = reader.read(ReadReq { pid, fds: false });
        if f.as_ref().is_some_and(|f| f.state == RunState::Zombie) {
            f
        } else {
            std::thread::sleep(std::time::Duration::from_millis(10));
            None
        }
    });
    let z = zombie.expect("hang detector: one minute");
    assert!(z.counters.is_none() && z.threads.is_none());
    assert_eq!(z.ppid, std::process::id());
    assert!(
        z.start.is_some_and(|s| s == f.start.unwrap_or(0)),
        "{z:?} {f:?}"
    );
    let sweep = crate::processes().expect("a sweep");
    let row = sweep
        .procs
        .iter()
        .find(|p| p.pid == pid)
        .expect("the zombie is swept");
    assert_eq!(
        (row.state, row.ppid),
        (RunState::Zombie, std::process::id())
    );
    child.wait().expect("reaped");
}

/// The sweep reads `exec` off the kernel's flag (macOS `P_EXEC` set, Linux `PF_FORKNOEXEC`
/// clear): `false` for a fork that kept its parent's program (zsh's `zpty` worker), `true` for a
/// program a fork ran (`sleep`).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_fork_that_kept_its_parents_program_ran_none_of_its_own() {
    let mut sleep = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("60")
        .spawn()
        .expect("sleep starts");
    // SAFETY: the child calls only `pause`, async-signal-safe, until the SIGKILL below.
    let fork = unsafe { libc::fork() };
    assert!(fork >= 0, "fork: {}", std::io::Error::last_os_error());
    if fork == 0 {
        loop {
            // SAFETY: async-signal-safe, with no state of the parent's touched.
            unsafe { libc::pause() };
        }
    }
    let sweep = crate::processes().expect("a sweep");
    let exec = |pid: u32| sweep.procs.iter().find(|p| p.pid == pid).map(|p| p.exec);
    let read = (exec(sleep.id()), exec(fork.cast_unsigned()));
    // SAFETY: our own child, killed and reaped once.
    unsafe {
        libc::kill(fork, libc::SIGKILL);
        libc::waitpid(fork, std::ptr::null_mut(), 0);
    }
    sleep.kill().expect("kill");
    sleep.wait().expect("reaped");
    assert_eq!(read, (Some(Some(true)), Some(Some(false))));
}

/// The job probe's CPU times are nanoseconds: a spinning child, stopped so every reading counts
/// the same CPU at any load, reads its counters and its threads' sum each within 2x of `wait4`'s.
/// `proc_pid_rusage` counts Mach ticks and `PROC_PIDTHREADINFO` nanoseconds; thread times scaled
/// as ticks read 125/3 too large (`docs/measured/rosetta-mac-2026-10-20.md` §3 (b)).
#[cfg(target_os = "macos")]
#[test]
fn a_stopped_childs_counters_and_threads_read_its_cpu_time_as_wait4_counts_it() {
    use crate::procs::stopped_spinner::{read, within_2x};
    let cpu = |pid: u32| {
        let f = JobsReader::new().read(ReadReq { pid, fds: false })?;
        let threads = f.threads?.iter().map(|t| t.cpu_ns).sum::<Option<u64>>()?;
        Some((f.counters?.cpu_ns, threads))
    };
    let (both, kernel) = read(|pid| cpu(pid).is_some_and(|(c, _)| c >= 50_000_000), cpu);
    let (counters, threads) = both.expect("the probe reads its own child");
    assert!(
        within_2x(counters, kernel) && within_2x(threads, kernel),
        "counters {counters} ns and threads {threads} ns; the reap's rusage for the same \
         stopped child is {kernel} ns (timebase {:?})",
        crate::procs::kernel_timebase()
    );
}

/// A start-suspended spawn reads one thread whose handle is `0`, stopped and untraced, so its
/// stop, not its age (read 21 s ahead), keeps it from reading held. The job probe reads the same
/// facts, and a reader handed a mount table places the executable by that table.
#[cfg(target_os = "macos")]
#[test]
fn a_start_suspended_child_reads_one_unstarted_thread_and_is_not_held() {
    let held =
        aterm_uds::deadline::Held::start(aterm_uds::spawnfd::command("/bin/sleep").arg("30"))
            .expect("a start-suspended sleep");
    let pid = held.id();
    let later = now_us() + 21_000_000;
    let facts = read_start(pid, later).expect("the child reads");
    assert!(
        facts.lone_zero && facts.stopped && !facts.traced && !facts.translated,
        "{facts:?}"
    );
    assert_eq!(
        facts.local,
        Some(true),
        "/bin is on the system volume: {facts:?}"
    );
    assert!(facts.age_ms >= Some(PROGRAM_CHECK_AGE_MS), "{facts:?}");
    assert_eq!(
        judge_start(&facts),
        AtStart::Started,
        "a start-suspended child is not held: {facts:?}"
    );
    assert_eq!(
        judge_start(&StartFacts {
            stopped: false,
            ..facts
        }),
        AtStart::Held,
        "its stop, not its age, is what excludes it"
    );
    let f = JobsReader::new()
        .read_all_at(&[ReadReq { pid, fds: false }], later)
        .pop()
        .expect("the job probe reads it");
    assert_eq!(f.state, RunState::Stopped, "{f:?}");
    assert_eq!(
        f.threads
            .as_deref()
            .map(|ts| ts.iter().map(|t| t.id).collect::<Vec<_>>()),
        Some(vec![0]),
        "one thread, its handle never set: {f:?}"
    );
    assert_eq!(
        f.start_facts,
        Some(facts),
        "the job probe reads the same start"
    );
    assert_eq!(f.program_check(), Some(AtStart::Started), "{f:?}");
    let now = JobsReader::new()
        .read(ReadReq { pid, fds: false })
        .expect("the job probe reads it");
    assert!(
        now.start_facts
            .is_some_and(|f| f.lone_zero && f.age_ms.is_some()),
        "the job probe's own clock reads the age: {now:?}"
    );
    let empty = std::cell::OnceCell::from(Table::default());
    assert_eq!(
        read_start_in(pid, later, &empty).map(|f| f.local),
        Some(None),
        "a handed table that holds no mount places nothing"
    );
    let me = read_start(std::process::id(), later).expect("this process reads");
    assert!(!me.lone_zero, "{me:?}");
    drop(held);
}

/// A program stopped by `SIGSTOP` after it ran keeps its threads' handles:
/// only a process whose thread never ran reads a lone `0`.
#[cfg(target_os = "macos")]
#[test]
fn a_program_stopped_after_it_ran_keeps_its_thread_handles() {
    use aterm_uds::spawnfd::Stdio;
    let mut cat = aterm_uds::spawnfd::command("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("cat starts");
    let stdin = aterm_uds::deadline::cat_echoes(&mut cat);
    let pid = cat.id();
    // SAFETY: our own child.
    assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGSTOP) }, 0);
    let mut reader = JobsReader::new();
    let stopped = (0..6000).find_map(|_| {
        let f = reader.read(ReadReq { pid, fds: false });
        if f.as_ref().is_some_and(|f| f.state == RunState::Stopped) {
            f
        } else {
            std::thread::sleep(std::time::Duration::from_millis(10));
            None
        }
    });
    let f = stopped.expect("hang detector: one minute");
    let facts = read_start(pid, now_us() + 21_000_000).expect("the child reads");
    assert!(!facts.lone_zero, "{facts:?} {f:?}");
    assert!(
        f.threads
            .as_deref()
            .is_some_and(|ts| !ts.is_empty() && ts.iter().all(|t| t.id != 0)),
        "{f:?}"
    );
    assert_eq!(f.start_facts, Some(StartFacts::default()), "{f:?}");
    assert_eq!(f.program_check(), Some(AtStart::Started), "{f:?}");
    cat.kill().expect("kill");
    drop(stdin);
    cat.wait().expect("reaped");
}

/// An unread start, or a change time before the epoch, makes no dylib fresh.
#[test]
fn a_fresh_dylib_is_one_written_at_most_an_hour_before_its_holder_started() {
    let start = 1_800_000_000_000_000u64;
    let hour = i64::try_from(FRESH_DYLIB_US / 1_000_000).expect("an hour in seconds");
    for (row, path, mtime, start, want) in [
        (
            "written since",
            &b"/t/libserde_derive-1.dylib"[..],
            1_800_000_100,
            Some(start),
            true,
        ),
        (
            "an hour before",
            b"/t/libserde_derive-1.dylib",
            1_800_000_000 - hour,
            Some(start),
            true,
        ),
        (
            "past the hour",
            b"/t/libserde_derive-1.dylib",
            1_800_000_000 - hour - 1,
            Some(start),
            false,
        ),
        (
            "no dylib",
            b"/t/libserde_derive-1.rlib",
            1_800_000_100,
            Some(start),
            false,
        ),
        (
            "start unread",
            b"/t/libserde_derive-1.dylib",
            1_800_000_100,
            None,
            false,
        ),
        (
            "before the epoch",
            b"/t/libserde_derive-1.dylib",
            -1,
            Some(start),
            false,
        ),
    ] {
        assert_eq!(fresh_dylib(path, mtime, start), want, "{row}");
    }
}

/// This process, which started before its scratch files were written, holds a `.dylib` written
/// now (fresh), a text file written now and a `.dylib` changed a day back (neither).
#[cfg(target_os = "macos")]
#[test]
fn a_members_descriptors_name_a_fresh_dylib_it_holds_open() {
    use std::os::fd::AsRawFd as _;
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let open = |name: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, b"\xcf\xfa\xed\xfe").expect("write a scratch file");
        std::fs::File::open(&path).expect("open it")
    };
    let fresh = open("libfresh.dylib");
    let text = open("notes.txt");
    let old = open("libold.dylib");
    std::fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join("libold.dylib"))
        .and_then(|f| {
            f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(86_400))
        })
        .expect("set its change time a day back");
    let f = JobsReader::new()
        .read(ReadReq {
            pid: std::process::id(),
            fds: true,
        })
        .expect("this process reads itself");
    let fds = f.fds.as_deref().expect("its descriptors read");
    let fresh_at = |fd: i32| {
        fds.iter().find(|r| r.fd == fd).map(|r| {
            matches!(
                r.kind,
                FdKind::File {
                    fresh_dylib: true,
                    ..
                }
            )
        })
    };
    assert_eq!(
        fresh_at(fresh.as_raw_fd()),
        Some(true),
        "a dylib written now: {fds:?}"
    );
    assert_eq!(fresh_at(text.as_raw_fd()), Some(false), "no dylib: {fds:?}");
    assert_eq!(fresh_at(old.as_raw_fd()), Some(false), "a day old: {fds:?}");
}

/// A job member's reading carries where it stands at its start: this
/// process runs.
#[cfg(target_os = "macos")]
#[test]
fn a_member_reading_carries_its_program_check() {
    let f = JobsReader::new()
        .read(ReadReq {
            pid: std::process::id(),
            fds: false,
        })
        .expect("this process reads itself");
    assert_eq!(f.start_facts, Some(StartFacts::default()), "{f:?}");
    assert_eq!(f.program_check(), Some(AtStart::Started), "{f:?}");
}

/// A pseudo-terminal's word is read through its device node, as its
/// reader in front last set it.
#[cfg(unix)]
#[test]
fn a_ptys_word_is_read_through_its_device_node() {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::fs::MetadataExt;
    let (mut m, mut s) = (-1, -1);
    // SAFETY: two writable ints; no name, mode or size is asked for.
    let rc = unsafe {
        libc::openpty(
            &raw mut m,
            &raw mut s,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: `openpty` opened both, and nothing else owns them.
    let (master, follower) = unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
    let node = std::fs::File::from(follower.try_clone().expect("dup"));
    let dev = u32::try_from(node.metadata().expect("fstat").rdev()).expect("a 32-bit device");
    let me = std::process::id();
    let word = term_word(me, dev).expect("aterm's user opens its own terminal");
    assert!(word.canonical && word.echo, "a new terminal's: {word:?}");
    // SAFETY: plain integers and bytes; all-zero is valid.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    let set = |t: &libc::termios| {
        // SAFETY: `follower` is open and `t` a whole `termios`.
        let rc = unsafe { libc::tcsetattr(follower.as_raw_fd(), libc::TCSANOW, t) };
        assert_eq!(rc, 0, "tcsetattr: {}", std::io::Error::last_os_error());
    };
    // SAFETY: `follower` is open and `t` a writable `termios`.
    assert_eq!(
        unsafe { libc::tcgetattr(follower.as_raw_fd(), &raw mut t) },
        0
    );
    t.c_lflag &= !libc::ECHO;
    set(&t);
    assert_eq!(
        term_word(me, dev),
        Some(TermWord {
            canonical: true,
            echo: false,
            queued: 0
        }),
        "a password prompt's"
    );
    // SAFETY: `t` is a whole `termios`.
    unsafe { libc::cfmakeraw(&raw mut t) };
    set(&t);
    assert_eq!(
        term_word(me, dev),
        Some(TermWord {
            canonical: false,
            echo: false,
            queued: 0
        }),
        "an editor's"
    );
    // Keys typed that the editor has not read queue in its terminal (Linux hands them on from a
    // worker: a hang detector, not a budget).
    // SAFETY: `master` is open and the three bytes live for the call.
    let n = unsafe { libc::write(master.as_raw_fd(), b"jjj".as_ptr().cast(), 3) };
    assert_eq!(n, 3, "write: {}", std::io::Error::last_os_error());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while term_word(me, dev).is_some_and(|w| w.queued < 3) && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(
        term_word(me, dev).map(|w| w.queued),
        Some(3),
        "the keys the editor has not read"
    );
}
