// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `libc` for `x86_64-unknown-linux-gnu`.
//!
//! GENERATED, and deliberately: every value below is a fact about this
//! platform's ABI, measured for THIS TRIPLE and checked against the
//! dev-dependency oracle. See `src/lib.rs` for the method and the
//! regeneration command. The only hand-written code in this file is the
//! bodies of the C macros libc reimplements in Rust and the inherent
//! siginfo_t accessors. The oracle audits their exact sets and checks
//! their behavior against the reference libc.

#[allow(unused_imports)]
use core::ffi::{
    c_char, c_double, c_float, c_int, c_long, c_longlong, c_schar, c_short, c_uchar, c_uint,
    c_ulong, c_ulonglong, c_ushort, c_void,
};

// ---------------------------------------------------------------- types
pub type __fsword_t = i64;
pub type __priority_which_t = c_uint;
pub type __rlimit_resource_t = c_uint;
pub type __u16 = c_ushort;
pub type __u32 = c_uint;
pub type __u64 = c_ulonglong;
pub type blkcnt_t = i64;
pub type blksize_t = i64;
pub type cc_t = c_uchar;
pub type clockid_t = c_int;
pub type dev_t = u64;
pub type fsblkcnt_t = u64;
pub type fsfilcnt_t = u64;
pub type gid_t = u32;
pub type id_t = c_uint;
pub type idtype_t = c_uint;
pub type in_addr_t = u32;
pub type in_port_t = u16;
pub type ino64_t = u64;
pub type ino_t = u64;
pub type mode_t = u32;
pub type nfds_t = c_ulong;
pub type nlink_t = u64;
pub type off64_t = i64;
pub type off_t = i64;
pub type pid_t = i32;
pub type pthread_t = c_ulong;
pub type rlim_t = u64;
pub type sa_family_t = u16;
pub type sighandler_t = size_t;
pub type size_t = usize;
pub type socklen_t = u32;
pub type speed_t = c_uint;
pub type ssize_t = isize;
pub type suseconds_t = i64;
pub type tcflag_t = c_uint;
pub type time_t = i64;
pub type uid_t = u32;
pub type wchar_t = i32;

/// Opaque to callers: libc exposes this as an uninhabited type.
#[derive(Clone, Copy, Debug)]
pub enum DIR {}

/// Opaque to callers: libc exposes this as an uninhabited type.
#[derive(Clone, Copy, Debug)]
pub enum FILE {}

/// Opaque to callers: libc exposes this as an uninhabited type.
#[derive(Clone, Copy, Debug)]
pub enum timezone {}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct cmsghdr {
    pub cmsg_len: size_t,
    pub cmsg_level: c_int,
    pub cmsg_type: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct dirent64 {
    pub d_ino: ino64_t,
    pub d_off: off64_t,
    pub d_reclen: c_ushort,
    pub d_type: c_uchar,
    pub d_name: [c_char; 256],
}

/// opaque in libc: 128 bytes, align 8
#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct fd_set {
    __opaque: [u64; 16],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct flock {
    pub l_type: c_short,
    pub l_whence: c_short,
    pub l_start: off_t,
    pub l_len: off_t,
    pub l_pid: pid_t,
}

/// opaque in libc: 8 bytes, align 4
#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct fsid_t {
    __opaque: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct if_nameindex {
    pub if_index: c_uint,
    pub if_name: *mut c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct ifaddrs {
    pub ifa_next: *mut ifaddrs,
    pub ifa_name: *mut c_char,
    pub ifa_flags: c_uint,
    pub ifa_addr: *mut sockaddr,
    pub ifa_netmask: *mut sockaddr,
    pub ifa_ifu: *mut sockaddr,
    pub ifa_data: *mut c_void,
}

#[repr(C, align(4))]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct in6_addr {
    pub s6_addr: [u8; 16],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct in_addr {
    pub s_addr: in_addr_t,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct iovec {
    pub iov_base: *mut c_void,
    pub iov_len: size_t,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct ip_mreq {
    pub imr_multiaddr: in_addr,
    pub imr_interface: in_addr,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct ipv6_mreq {
    pub ipv6mr_multiaddr: in6_addr,
    pub ipv6mr_interface: c_uint,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct linger {
    pub l_onoff: c_int,
    pub l_linger: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct msghdr {
    pub msg_name: *mut c_void,
    pub msg_namelen: socklen_t,
    pub msg_iov: *mut iovec,
    pub msg_iovlen: size_t,
    pub msg_control: *mut c_void,
    pub msg_controllen: size_t,
    pub msg_flags: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct open_how {
    pub flags: __u64,
    pub mode: __u64,
    pub resolve: __u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct passwd {
    pub pw_name: *mut c_char,
    pub pw_passwd: *mut c_char,
    pub pw_uid: uid_t,
    pub pw_gid: gid_t,
    pub pw_gecos: *mut c_char,
    pub pw_dir: *mut c_char,
    pub pw_shell: *mut c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct pollfd {
    pub fd: c_int,
    pub events: c_short,
    pub revents: c_short,
}

/// opaque in libc: 48 bytes, align 8
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct pthread_cond_t {
    __opaque: [u64; 6],
}

/// opaque in libc: 4 bytes, align 4
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct pthread_condattr_t {
    __opaque: [u32; 1],
}

/// opaque in libc: 40 bytes, align 8
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct pthread_mutex_t {
    __opaque: [u64; 5],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct rlimit {
    pub rlim_cur: rlim_t,
    pub rlim_max: rlim_t,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
#[allow(unpredictable_function_pointer_comparisons)]
pub struct sigaction {
    pub sa_sigaction: sighandler_t,
    pub sa_mask: sigset_t,
    pub sa_flags: c_int,
    pub sa_restorer: Option<extern "C" fn()>,
}

/// libc keeps private fields here; the __pad members reproduce their measured extent (total 128 bytes, align 8)
/// alignment stated explicitly: libc gets it from a private field whose type is not part of the contract
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct siginfo_t {
    pub si_signo: c_int,
    pub si_errno: c_int,
    pub si_code: c_int,
    __pad0: [u8; 116],
}

/// opaque in libc: 128 bytes, align 8
#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sigset_t {
    __opaque: [u64; 16],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sigval {
    pub sival_ptr: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct sock_txtime {
    pub clockid: clockid_t,
    pub flags: __u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sockaddr {
    pub sa_family: sa_family_t,
    pub sa_data: [c_char; 14],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sockaddr_alg {
    pub salg_family: sa_family_t,
    pub salg_type: [c_uchar; 14],
    pub salg_feat: u32,
    pub salg_mask: u32,
    pub salg_name: [c_uchar; 64],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sockaddr_in {
    pub sin_family: sa_family_t,
    pub sin_port: in_port_t,
    pub sin_addr: in_addr,
    pub sin_zero: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sockaddr_in6 {
    pub sin6_family: sa_family_t,
    pub sin6_port: in_port_t,
    pub sin6_flowinfo: u32,
    pub sin6_addr: in6_addr,
    pub sin6_scope_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sockaddr_ll {
    pub sll_family: c_ushort,
    pub sll_protocol: c_ushort,
    pub sll_ifindex: c_int,
    pub sll_hatype: c_ushort,
    pub sll_pkttype: c_uchar,
    pub sll_halen: c_uchar,
    pub sll_addr: [c_uchar; 8],
}

/// libc keeps private fields here; the __pad members reproduce their measured extent (total 12 bytes, align 4)
#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sockaddr_nl {
    pub nl_family: sa_family_t,
    pub nl_pid: u32,
    pub nl_groups: u32,
}

/// libc keeps private fields here; the __pad members reproduce their measured extent (total 128 bytes, align 8)
/// alignment stated explicitly: libc gets it from a private field whose type is not part of the contract
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sockaddr_storage {
    pub ss_family: sa_family_t,
    __pad0: [u8; 126],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sockaddr_un {
    pub sun_family: sa_family_t,
    pub sun_path: [c_char; 108],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sockaddr_vm {
    pub svm_family: sa_family_t,
    pub svm_reserved1: c_ushort,
    pub svm_port: c_uint,
    pub svm_cid: c_uint,
    pub svm_zero: [u8; 4],
}

/// libc keeps private fields here; the __pad members reproduce their measured extent (total 144 bytes, align 8)
#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct stat {
    pub st_dev: dev_t,
    pub st_ino: ino_t,
    pub st_nlink: nlink_t,
    pub st_mode: mode_t,
    pub st_uid: uid_t,
    pub st_gid: gid_t,
    pub st_rdev: dev_t,
    pub st_size: off_t,
    pub st_blksize: blksize_t,
    pub st_blocks: blkcnt_t,
    pub st_atime: time_t,
    pub st_atime_nsec: i64,
    pub st_mtime: time_t,
    pub st_mtime_nsec: i64,
    pub st_ctime: time_t,
    pub st_ctime_nsec: i64,
    __pad0: [u8; 24],
}

/// libc keeps private fields here; the __pad members reproduce their measured extent (total 120 bytes, align 8)
#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct statfs {
    pub f_type: __fsword_t,
    pub f_bsize: __fsword_t,
    pub f_blocks: fsblkcnt_t,
    pub f_bfree: fsblkcnt_t,
    pub f_bavail: fsblkcnt_t,
    pub f_files: fsfilcnt_t,
    pub f_ffree: fsfilcnt_t,
    pub f_fsid: fsid_t,
    pub f_namelen: __fsword_t,
    pub f_frsize: __fsword_t,
    __pad0: [u8; 40],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct statfs64 {
    pub f_type: __fsword_t,
    pub f_bsize: __fsword_t,
    pub f_blocks: u64,
    pub f_bfree: u64,
    pub f_bavail: u64,
    pub f_files: u64,
    pub f_ffree: u64,
    pub f_fsid: fsid_t,
    pub f_namelen: __fsword_t,
    pub f_frsize: __fsword_t,
    pub f_flags: __fsword_t,
    pub f_spare: [__fsword_t; 4],
}

/// libc keeps private fields here; the __pad members reproduce their measured extent (total 112 bytes, align 8)
#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct statvfs {
    pub f_bsize: c_ulong,
    pub f_frsize: c_ulong,
    pub f_blocks: fsblkcnt_t,
    pub f_bfree: fsblkcnt_t,
    pub f_bavail: fsblkcnt_t,
    pub f_files: fsfilcnt_t,
    pub f_ffree: fsfilcnt_t,
    pub f_favail: fsfilcnt_t,
    pub f_fsid: c_ulong,
    pub f_flag: c_ulong,
    pub f_namemax: c_ulong,
    __pad0: [u8; 24],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sysinfo {
    pub uptime: i64,
    pub loads: [u64; 3],
    pub totalram: u64,
    pub freeram: u64,
    pub sharedram: u64,
    pub bufferram: u64,
    pub totalswap: u64,
    pub freeswap: u64,
    pub procs: c_ushort,
    pub pad: c_ushort,
    pub totalhigh: u64,
    pub freehigh: u64,
    pub mem_unit: c_uint,
    pub _f: [c_char; 0],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct termios {
    pub c_iflag: tcflag_t,
    pub c_oflag: tcflag_t,
    pub c_cflag: tcflag_t,
    pub c_lflag: tcflag_t,
    pub c_line: cc_t,
    pub c_cc: [cc_t; 32],
    pub c_ispeed: speed_t,
    pub c_ospeed: speed_t,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct timespec {
    pub tv_sec: time_t,
    pub tv_nsec: c_long,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct timeval {
    pub tv_sec: time_t,
    pub tv_usec: suseconds_t,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct tls12_crypto_info_aes_gcm_128 {
    pub info: tls_crypto_info,
    pub iv: [c_uchar; 8],
    pub key: [c_uchar; 16],
    pub salt: [c_uchar; 4],
    pub rec_seq: [c_uchar; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct tls12_crypto_info_aes_gcm_256 {
    pub info: tls_crypto_info,
    pub iv: [c_uchar; 8],
    pub key: [c_uchar; 32],
    pub salt: [c_uchar; 4],
    pub rec_seq: [c_uchar; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct tls12_crypto_info_chacha20_poly1305 {
    pub info: tls_crypto_info,
    pub iv: [c_uchar; 12],
    pub key: [c_uchar; 32],
    pub salt: [c_uchar; 0],
    pub rec_seq: [c_uchar; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct tls_crypto_info {
    pub version: __u16,
    pub cipher_type: __u16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct ucred {
    pub pid: pid_t,
    pub uid: uid_t,
    pub gid: gid_t,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct winsize {
    pub ws_row: c_ushort,
    pub ws_col: c_ushort,
    pub ws_xpixel: c_ushort,
    pub ws_ypixel: c_ushort,
}

// ------------------------------------------------------------ constants
pub const ADFS_SUPER_MAGIC: c_long = 0x0000adf5;
pub const AFFS_SUPER_MAGIC: c_long = 0x0000adff;
pub const AFS_SUPER_MAGIC: c_long = 0x5346414f;
pub const AF_ALG: c_int = 38;
pub const AF_APPLETALK: c_int = 5;
pub const AF_ASH: c_int = 18;
pub const AF_ATMPVC: c_int = 8;
pub const AF_ATMSVC: c_int = 20;
pub const AF_AX25: c_int = 3;
pub const AF_BLUETOOTH: c_int = 31;
pub const AF_BRIDGE: c_int = 7;
pub const AF_CAIF: c_int = 37;
pub const AF_CAN: c_int = 29;
pub const AF_DECnet: c_int = 12;
pub const AF_ECONET: c_int = 19;
pub const AF_IB: c_int = 27;
pub const AF_IEEE802154: c_int = 36;
pub const AF_INET: c_int = 2;
pub const AF_INET6: c_int = 10;
pub const AF_IPX: c_int = 4;
pub const AF_IRDA: c_int = 23;
pub const AF_ISDN: c_int = 34;
pub const AF_IUCV: c_int = 32;
pub const AF_KEY: c_int = 15;
pub const AF_LLC: c_int = 26;
pub const AF_MPLS: c_int = 28;
pub const AF_NETBEUI: c_int = 13;
pub const AF_NETLINK: c_int = 16;
pub const AF_NETROM: c_int = 6;
pub const AF_NFC: c_int = 39;
pub const AF_PACKET: c_int = 17;
pub const AF_PHONET: c_int = 35;
pub const AF_PPPOX: c_int = 24;
pub const AF_RDS: c_int = 21;
pub const AF_ROSE: c_int = 11;
pub const AF_RXRPC: c_int = 33;
pub const AF_SECURITY: c_int = 14;
pub const AF_SNA: c_int = 22;
pub const AF_TIPC: c_int = 30;
pub const AF_UNIX: c_int = 1;
pub const AF_UNSPEC: c_int = 0;
pub const AF_VSOCK: c_int = 40;
pub const AF_WANPIPE: c_int = 25;
pub const AF_X25: c_int = 9;
pub const ALG_SET_AEAD_AUTHSIZE: c_int = 5;
pub const ALG_SET_KEY: c_int = 1;
pub const AT_EACCESS: c_int = 0x200;
pub const AT_EMPTY_PATH: c_int = 0x1000;
pub const AT_FDCWD: c_int = -100;
pub const AT_NO_AUTOMOUNT: c_int = 0x800;
pub const AT_REMOVEDIR: c_int = 0x200;
pub const AT_SYMLINK_FOLLOW: c_int = 0x400;
pub const AT_SYMLINK_NOFOLLOW: c_int = 0x100;
pub const AUTOFS_SUPER_MAGIC: c_long = 0x0187;
pub const B230400: speed_t = 0o010003;
pub const BPF_FS_MAGIC: c_long = 0xcafe4a11;
pub const BRKINT: tcflag_t = 0x00000002;
pub const BTRFS_SUPER_MAGIC: c_long = 0x9123683e;
pub const CGROUP2_SUPER_MAGIC: c_long = 0x63677270;
pub const CGROUP_SUPER_MAGIC: c_long = 0x27e0eb;
pub const CLD_CONTINUED: c_int = 6;
pub const CLD_DUMPED: c_int = 3;
pub const CLD_EXITED: c_int = 1;
pub const CLD_KILLED: c_int = 2;
pub const CLD_STOPPED: c_int = 5;
pub const CLD_TRAPPED: c_int = 4;
pub const CLOCAL: tcflag_t = 0x00000800;
pub const CLOCK_MONOTONIC: clockid_t = 1;
pub const CLOCK_PROCESS_CPUTIME_ID: clockid_t = 2;
pub const CLOCK_REALTIME: clockid_t = 0;
pub const CLOCK_THREAD_CPUTIME_ID: clockid_t = 3;
pub const CODA_SUPER_MAGIC: c_long = 0x73757245;
pub const CRAMFS_MAGIC: c_long = 0x28cd3d45;
pub const CREAD: tcflag_t = 0x00000080;
pub const CRTSCTS: tcflag_t = 0x80000000;
pub const CS5: tcflag_t = 0x00000000;
pub const CS6: tcflag_t = 0x00000010;
pub const CS7: tcflag_t = 0x00000020;
pub const CS8: tcflag_t = 0x00000030;
pub const CSIZE: tcflag_t = 0x00000030;
pub const CSTOPB: tcflag_t = 0x00000040;
pub const CTL_KERN: c_int = 1;
pub const DEBUGFS_MAGIC: c_long = 0x64626720;
pub const DEVPTS_SUPER_MAGIC: c_long = 0x1cd1;
pub const DT_BLK: u8 = 6;
pub const DT_CHR: u8 = 2;
pub const DT_DIR: u8 = 4;
pub const DT_FIFO: u8 = 1;
pub const DT_LNK: u8 = 10;
pub const DT_REG: u8 = 8;
pub const DT_SOCK: u8 = 12;
pub const E2BIG: c_int = 7;
pub const EACCES: c_int = 13;
pub const EADDRINUSE: c_int = 98;
pub const EADDRNOTAVAIL: c_int = 99;
pub const EADV: c_int = 68;
pub const EAFNOSUPPORT: c_int = 97;
pub const EAGAIN: c_int = 11;
pub const EALREADY: c_int = 114;
pub const EBADE: c_int = 52;
pub const EBADF: c_int = 9;
pub const EBADFD: c_int = 77;
pub const EBADMSG: c_int = 74;
pub const EBADR: c_int = 53;
pub const EBADRQC: c_int = 56;
pub const EBADSLT: c_int = 57;
pub const EBFONT: c_int = 59;
pub const EBUSY: c_int = 16;
pub const ECANCELED: c_int = 125;
pub const ECHILD: c_int = 10;
pub const ECHO: tcflag_t = 0x00000008;
pub const ECHOCTL: tcflag_t = 0x00000200;
pub const ECHOE: tcflag_t = 0x00000010;
pub const ECHOK: tcflag_t = 0x00000020;
pub const ECHOKE: tcflag_t = 0x00000800;
pub const ECHONL: tcflag_t = 0x00000040;
pub const ECHOPRT: tcflag_t = 0x00000400;
pub const ECHRNG: c_int = 44;
pub const ECOMM: c_int = 70;
pub const ECONNABORTED: c_int = 103;
pub const ECONNREFUSED: c_int = 111;
pub const ECONNRESET: c_int = 104;
pub const ECRYPTFS_SUPER_MAGIC: c_long = 0xf15f;
pub const EDEADLK: c_int = 35;
pub const EDESTADDRREQ: c_int = 89;
pub const EDOM: c_int = 33;
pub const EDOTDOT: c_int = 73;
pub const EDQUOT: c_int = 122;
pub const EEXIST: c_int = 17;
pub const EFAULT: c_int = 14;
pub const EFBIG: c_int = 27;
pub const EFS_SUPER_MAGIC: c_long = 0x00414a53;
pub const EHOSTDOWN: c_int = 112;
pub const EHOSTUNREACH: c_int = 113;
pub const EHWPOISON: c_int = 133;
pub const EIDRM: c_int = 43;
pub const EILSEQ: c_int = 84;
pub const EINPROGRESS: c_int = 115;
pub const EINTR: c_int = 4;
pub const EINVAL: c_int = 22;
pub const EIO: c_int = 5;
pub const EISCONN: c_int = 106;
pub const EISDIR: c_int = 21;
pub const EISNAM: c_int = 120;
pub const EKEYEXPIRED: c_int = 127;
pub const EKEYREJECTED: c_int = 129;
pub const EKEYREVOKED: c_int = 128;
pub const EL2HLT: c_int = 51;
pub const EL2NSYNC: c_int = 45;
pub const EL3HLT: c_int = 46;
pub const EL3RST: c_int = 47;
pub const ELIBACC: c_int = 79;
pub const ELIBBAD: c_int = 80;
pub const ELIBEXEC: c_int = 83;
pub const ELIBMAX: c_int = 82;
pub const ELIBSCN: c_int = 81;
pub const ELNRNG: c_int = 48;
pub const ELOOP: c_int = 40;
pub const EMEDIUMTYPE: c_int = 124;
pub const EMFILE: c_int = 24;
pub const EMLINK: c_int = 31;
pub const EMSGSIZE: c_int = 90;
pub const EMULTIHOP: c_int = 72;
pub const ENAMETOOLONG: c_int = 36;
pub const ENAVAIL: c_int = 119;
pub const ENETDOWN: c_int = 100;
pub const ENETRESET: c_int = 102;
pub const ENETUNREACH: c_int = 101;
pub const ENFILE: c_int = 23;
pub const ENOANO: c_int = 55;
pub const ENOATTR: c_int = 61;
pub const ENOBUFS: c_int = 105;
pub const ENOCSI: c_int = 50;
pub const ENODATA: c_int = 61;
pub const ENODEV: c_int = 19;
pub const ENOENT: c_int = 2;
pub const ENOEXEC: c_int = 8;
pub const ENOKEY: c_int = 126;
pub const ENOLCK: c_int = 37;
pub const ENOLINK: c_int = 67;
pub const ENOMEDIUM: c_int = 123;
pub const ENOMEM: c_int = 12;
pub const ENOMSG: c_int = 42;
pub const ENONET: c_int = 64;
pub const ENOPKG: c_int = 65;
pub const ENOPROTOOPT: c_int = 92;
pub const ENOSPC: c_int = 28;
pub const ENOSR: c_int = 63;
pub const ENOSTR: c_int = 60;
pub const ENOSYS: c_int = 38;
pub const ENOTBLK: c_int = 15;
pub const ENOTCONN: c_int = 107;
pub const ENOTDIR: c_int = 20;
pub const ENOTEMPTY: c_int = 39;
pub const ENOTNAM: c_int = 118;
pub const ENOTRECOVERABLE: c_int = 131;
pub const ENOTSOCK: c_int = 88;
pub const ENOTSUP: c_int = 95;
pub const ENOTTY: c_int = 25;
pub const ENOTUNIQ: c_int = 76;
pub const ENXIO: c_int = 6;
pub const EOPNOTSUPP: c_int = 95;
pub const EOVERFLOW: c_int = 75;
pub const EOWNERDEAD: c_int = 130;
pub const EPERM: c_int = 1;
pub const EPFNOSUPPORT: c_int = 96;
pub const EPIPE: c_int = 32;
pub const EPROTO: c_int = 71;
pub const EPROTONOSUPPORT: c_int = 93;
pub const EPROTOTYPE: c_int = 91;
pub const ERANGE: c_int = 34;
pub const EREMCHG: c_int = 78;
pub const EREMOTE: c_int = 66;
pub const EREMOTEIO: c_int = 121;
pub const ERESTART: c_int = 85;
pub const ERFKILL: c_int = 132;
pub const EROFS: c_int = 30;
pub const ESHUTDOWN: c_int = 108;
pub const ESOCKTNOSUPPORT: c_int = 94;
pub const ESPIPE: c_int = 29;
pub const ESRCH: c_int = 3;
pub const ESRMNT: c_int = 69;
pub const ESTALE: c_int = 116;
pub const ESTRPIPE: c_int = 86;
pub const ETH_P_ALL: c_int = 0x0003;
pub const ETIME: c_int = 62;
pub const ETIMEDOUT: c_int = 110;
pub const ETOOMANYREFS: c_int = 109;
pub const ETXTBSY: c_int = 26;
pub const EUCLEAN: c_int = 117;
pub const EUNATCH: c_int = 49;
pub const EUSERS: c_int = 87;
pub const EWOULDBLOCK: c_int = 11;
pub const EXDEV: c_int = 18;
pub const EXFULL: c_int = 54;
pub const EXIT_FAILURE: c_int = 1;
pub const EXIT_SUCCESS: c_int = 0;
pub const EXT2_SUPER_MAGIC: c_long = 0x0000ef53;
pub const EXT3_SUPER_MAGIC: c_long = 0x0000ef53;
pub const EXT4_SUPER_MAGIC: c_long = 0x0000ef53;
pub const EXTPROC: tcflag_t = 0x00010000;
pub const F2FS_SUPER_MAGIC: c_long = 0xf2f52010;
pub const FALLOC_FL_COLLAPSE_RANGE: c_int = 0x08;
pub const FALLOC_FL_INSERT_RANGE: c_int = 0x20;
pub const FALLOC_FL_KEEP_SIZE: c_int = 0x01;
pub const FALLOC_FL_PUNCH_HOLE: c_int = 0x02;
pub const FALLOC_FL_UNSHARE_RANGE: c_int = 0x40;
pub const FALLOC_FL_ZERO_RANGE: c_int = 0x10;
pub const FD_CLOEXEC: c_int = 0x1;
pub const FD_SETSIZE: usize = 1024;
pub const FIOCLEX: c_ulong = 0x5451;
pub const FIONBIO: c_ulong = 0x5421;
pub const FIONCLEX: c_ulong = 0x5450;
pub const FIONREAD: c_ulong = 0x541B;
pub const FLUSHO: tcflag_t = 0x00001000;
pub const FUSE_SUPER_MAGIC: c_long = 0x65735546;
pub const FUTEXFS_SUPER_MAGIC: c_long = 0xbad1dea;
pub const FUTEX_PRIVATE_FLAG: c_int = 128;
pub const FUTEX_WAIT: c_int = 0;
pub const FUTEX_WAKE: c_int = 1;
pub const F_ADD_SEALS: c_int = 1033;
pub const F_DUPFD: c_int = 0;
pub const F_DUPFD_CLOEXEC: c_int = 1030;
pub const F_GETFD: c_int = 1;
pub const F_GETFL: c_int = 3;
pub const F_GETLK: c_int = 5;
pub const F_GETPIPE_SZ: c_int = 1032;
pub const F_GET_SEALS: c_int = 1034;
pub const F_OFD_GETLK: c_int = 36;
pub const F_OFD_SETLK: c_int = 37;
pub const F_OFD_SETLKW: c_int = 38;
pub const F_OK: c_int = 0;
pub const F_RDLCK: c_int = 0;
pub const F_SEAL_FUTURE_WRITE: c_int = 0x0010;
pub const F_SEAL_GROW: c_int = 0x0004;
pub const F_SEAL_SEAL: c_int = 0x0001;
pub const F_SEAL_SHRINK: c_int = 0x0002;
pub const F_SEAL_WRITE: c_int = 0x0008;
pub const F_SETFD: c_int = 2;
pub const F_SETFL: c_int = 4;
pub const F_SETLK: c_int = 6;
pub const F_SETLKW: c_int = 7;
pub const F_SETPIPE_SZ: c_int = 1031;
pub const F_UNLCK: c_int = 2;
pub const F_WRLCK: c_int = 1;
pub const GRND_RANDOM: c_uint = 0x0002;
pub const HOSTFS_SUPER_MAGIC: c_long = 0x00c0ffee;
pub const HPFS_SUPER_MAGIC: c_long = 0xf995e849;
pub const HUGETLBFS_MAGIC: c_long = 0x958458f6;
pub const HUPCL: tcflag_t = 0x00000400;
pub const ICANON: tcflag_t = 0x00000002;
pub const ICRNL: tcflag_t = 0x00000100;
pub const IEXTEN: tcflag_t = 0x00008000;
pub const IFF_ALLMULTI: c_int = 0x200;
pub const IFF_AUTOMEDIA: c_int = 0x4000;
pub const IFF_BROADCAST: c_int = 0x2;
pub const IFF_DEBUG: c_int = 0x4;
pub const IFF_DORMANT: c_int = 0x20000;
pub const IFF_DYNAMIC: c_int = 0x8000;
pub const IFF_ECHO: c_int = 0x40000;
pub const IFF_LOOPBACK: c_int = 0x8;
pub const IFF_LOWER_UP: c_int = 0x10000;
pub const IFF_MASTER: c_int = 0x400;
pub const IFF_MULTICAST: c_int = 0x1000;
pub const IFF_NOARP: c_int = 0x80;
pub const IFF_NOTRAILERS: c_int = 0x20;
pub const IFF_NO_PI: c_int = 0x1000;
pub const IFF_POINTOPOINT: c_int = 0x10;
pub const IFF_PORTSEL: c_int = 0x2000;
pub const IFF_PROMISC: c_int = 0x100;
pub const IFF_RUNNING: c_int = 0x40;
pub const IFF_SLAVE: c_int = 0x800;
pub const IFF_TAP: c_int = 0x0002;
pub const IFF_TUN: c_int = 0x0001;
pub const IFF_UP: c_int = 0x1;
pub const IFNAMSIZ: size_t = 16;
pub const IF_NAMESIZE: size_t = 16;
pub const IGNBRK: tcflag_t = 0x00000001;
pub const IGNCR: tcflag_t = 0x00000080;
pub const IGNPAR: tcflag_t = 0x00000004;
pub const IMAXBEL: tcflag_t = 0x00002000;
pub const INLCR: tcflag_t = 0x00000040;
pub const INPCK: tcflag_t = 0x00000010;
pub const INT_MAX: c_int = 2147483647;
pub const IP6T_SO_ORIGINAL_DST: c_int = 80;
pub const IPPROTO_ICMP: c_int = 1;
pub const IPPROTO_ICMPV6: c_int = 58;
pub const IPPROTO_IP: c_int = 0;
pub const IPPROTO_IPV6: c_int = 41;
pub const IPPROTO_RAW: c_int = 255;
pub const IPPROTO_TCP: c_int = 6;
pub const IPPROTO_UDP: c_int = 17;
pub const IPV6_ADD_MEMBERSHIP: c_int = 20;
pub const IPV6_DONTFRAG: c_int = 62;
pub const IPV6_DROP_MEMBERSHIP: c_int = 21;
pub const IPV6_MULTICAST_HOPS: c_int = 18;
pub const IPV6_ORIGDSTADDR: c_int = 74;
pub const IPV6_RECVERR: c_int = 25;
pub const IPV6_RECVPKTINFO: c_int = 49;
pub const IPV6_TCLASS: c_int = 67;
pub const IPV6_UNICAST_HOPS: c_int = 16;
pub const IPV6_V6ONLY: c_int = 26;
pub const IP_ADD_MEMBERSHIP: c_int = 35;
pub const IP_BIND_ADDRESS_NO_PORT: c_int = 24;
pub const IP_DROP_MEMBERSHIP: c_int = 36;
pub const IP_FREEBIND: c_int = 15;
pub const IP_MTU: c_int = 14;
pub const IP_MULTICAST_LOOP: c_int = 34;
pub const IP_MULTICAST_TTL: c_int = 33;
pub const IP_ORIGDSTADDR: c_int = 20;
pub const IP_PKTINFO: c_int = 8;
pub const IP_RECVERR: c_int = 11;
pub const IP_TOS: c_int = 1;
pub const IP_TRANSPARENT: c_int = 19;
pub const IP_TTL: c_int = 2;
pub const ISIG: tcflag_t = 0x00000001;
pub const ISOFS_SUPER_MAGIC: c_long = 0x00009660;
pub const ISTRIP: tcflag_t = 0x00000020;
pub const IUTF8: tcflag_t = 0x00004000;
pub const IXANY: tcflag_t = 0x00000800;
pub const IXOFF: tcflag_t = 0x00001000;
pub const IXON: tcflag_t = 0x00000400;
pub const JFFS2_SUPER_MAGIC: c_long = 0x000072b6;
pub const LC_CTYPE: c_int = 0;
pub const LOCK_EX: c_int = 2;
pub const LOCK_NB: c_int = 4;
pub const LOCK_SH: c_int = 1;
pub const LOCK_UN: c_int = 8;
pub const MADV_DODUMP: c_int = 17;
pub const MADV_DOFORK: c_int = 11;
pub const MADV_DONTDUMP: c_int = 16;
pub const MADV_DONTFORK: c_int = 10;
pub const MADV_DONTNEED: c_int = 4;
pub const MADV_FREE: c_int = 8;
pub const MADV_HUGEPAGE: c_int = 14;
pub const MADV_HWPOISON: c_int = 100;
pub const MADV_MERGEABLE: c_int = 12;
pub const MADV_NOHUGEPAGE: c_int = 15;
pub const MADV_NORMAL: c_int = 0;
pub const MADV_POPULATE_READ: c_int = 22;
pub const MADV_POPULATE_WRITE: c_int = 23;
pub const MADV_RANDOM: c_int = 1;
pub const MADV_REMOVE: c_int = 9;
pub const MADV_SEQUENTIAL: c_int = 2;
pub const MADV_SOFT_OFFLINE: c_int = 101;
pub const MADV_UNMERGEABLE: c_int = 13;
pub const MADV_WILLNEED: c_int = 3;
pub const MAP_32BIT: c_int = 0x0040;
pub const MAP_ANON: c_int = 0x0020;
pub const MAP_ANONYMOUS: c_int = 0x0020;
pub const MAP_DENYWRITE: c_int = 0x0800;
pub const MAP_EXECUTABLE: c_int = 0x01000;
pub const MAP_FAILED: *mut c_void = 0xffffffffffffffff as *mut c_void;
pub const MAP_FILE: c_int = 0x0000;
pub const MAP_FIXED: c_int = 0x0010;
pub const MAP_FIXED_NOREPLACE: c_int = 0x100000;
pub const MAP_GROWSDOWN: c_int = 0x0100;
pub const MAP_HUGETLB: c_int = 0x040000;
pub const MAP_HUGE_16GB: c_int = -2013265920;
pub const MAP_HUGE_16MB: c_int = 1610612736;
pub const MAP_HUGE_1GB: c_int = 2013265920;
pub const MAP_HUGE_1MB: c_int = 1342177280;
pub const MAP_HUGE_256MB: c_int = 1879048192;
pub const MAP_HUGE_2GB: c_int = 2080374784;
pub const MAP_HUGE_2MB: c_int = 1409286144;
pub const MAP_HUGE_32MB: c_int = 1677721600;
pub const MAP_HUGE_512KB: c_int = 1275068416;
pub const MAP_HUGE_512MB: c_int = 1946157056;
pub const MAP_HUGE_64KB: c_int = 1073741824;
pub const MAP_HUGE_8MB: c_int = 1543503872;
pub const MAP_HUGE_MASK: c_int = 63;
pub const MAP_HUGE_SHIFT: c_int = 26;
pub const MAP_LOCKED: c_int = 0x02000;
pub const MAP_NONBLOCK: c_int = 0x010000;
pub const MAP_NORESERVE: c_int = 0x04000;
pub const MAP_POPULATE: c_int = 0x08000;
pub const MAP_PRIVATE: c_int = 0x0002;
pub const MAP_SHARED: c_int = 0x0001;
pub const MAP_STACK: c_int = 0x020000;
pub const MCL_CURRENT: c_int = 0x0001;
pub const MCL_FUTURE: c_int = 0x0002;
pub const MFD_ALLOW_SEALING: c_uint = 0x0002;
pub const MFD_CLOEXEC: c_uint = 0x0001;
pub const MFD_HUGETLB: c_uint = 0x0004;
pub const MFD_HUGE_16GB: c_uint = 0x88000000;
pub const MFD_HUGE_16MB: c_uint = 0x60000000;
pub const MFD_HUGE_1GB: c_uint = 0x78000000;
pub const MFD_HUGE_1MB: c_uint = 0x50000000;
pub const MFD_HUGE_256MB: c_uint = 0x70000000;
pub const MFD_HUGE_2GB: c_uint = 0x7c000000;
pub const MFD_HUGE_2MB: c_uint = 0x54000000;
pub const MFD_HUGE_32MB: c_uint = 0x64000000;
pub const MFD_HUGE_512MB: c_uint = 0x74000000;
pub const MFD_HUGE_8MB: c_uint = 0x5c000000;
pub const MINIX2_SUPER_MAGIC: c_long = 0x00002468;
pub const MINIX2_SUPER_MAGIC2: c_long = 0x00002478;
pub const MINIX3_SUPER_MAGIC: c_long = 0x4d5a;
pub const MINIX_SUPER_MAGIC: c_long = 0x0000137f;
pub const MINIX_SUPER_MAGIC2: c_long = 0x0000138f;
pub const MREMAP_FIXED: c_int = 2;
pub const MREMAP_MAYMOVE: c_int = 1;
pub const MSDOS_SUPER_MAGIC: c_long = 0x00004d44;
pub const MSG_CMSG_CLOEXEC: c_int = 0x40000000;
pub const MSG_CTRUNC: c_int = 8;
pub const MSG_DONTWAIT: c_int = 0x40;
pub const MSG_EOR: c_int = 0x80;
pub const MSG_ERRQUEUE: c_int = 0x2000;
pub const MSG_NOSIGNAL: c_int = 0x4000;
pub const MSG_OOB: c_int = 1;
pub const MSG_PEEK: c_int = 2;
pub const MSG_TRUNC: c_int = 0x20;
pub const MSG_WAITALL: c_int = 0x100;
pub const MSG_WAITFORONE: c_int = 0x10000;
pub const MS_ASYNC: c_int = 0x0001;
pub const MS_INVALIDATE: c_int = 0x0002;
pub const MS_SYNC: c_int = 0x0004;
pub const NCCS: usize = 32;
pub const NCP_SUPER_MAGIC: c_long = 0x0000564c;
pub const NETLINK_AUDIT: c_int = 9;
pub const NETLINK_CRYPTO: c_int = 21;
pub const NETLINK_DNRTMSG: c_int = 14;
pub const NETLINK_FIB_LOOKUP: c_int = 10;
pub const NETLINK_GENERIC: c_int = 16;
pub const NETLINK_IP6_FW: c_int = 13;
pub const NETLINK_ISCSI: c_int = 8;
pub const NETLINK_KOBJECT_UEVENT: c_int = 15;
pub const NETLINK_NETFILTER: c_int = 12;
pub const NETLINK_NFLOG: c_int = 5;
pub const NETLINK_RDMA: c_int = 20;
pub const NETLINK_ROUTE: c_int = 0;
pub const NETLINK_SCSITRANSPORT: c_int = 18;
pub const NETLINK_SELINUX: c_int = 7;
pub const NETLINK_SOCK_DIAG: c_int = 4;
pub const NETLINK_USERSOCK: c_int = 2;
pub const NFS_SUPER_MAGIC: c_long = 0x00006969;
pub const NILFS_SUPER_MAGIC: c_long = 0x3434;
pub const NOFLSH: tcflag_t = 0x00000080;
pub const NSFS_MAGIC: c_long = 0x6e736673;
pub const OCFS2_SUPER_MAGIC: c_long = 0x7461636f;
pub const OCRNL: tcflag_t = 0o000010;
pub const ONLCR: tcflag_t = 0x4;
pub const ONLRET: tcflag_t = 0o000040;
pub const ONOCR: tcflag_t = 0o000020;
pub const OPENPROM_SUPER_MAGIC: c_long = 0x00009fa1;
pub const OPOST: tcflag_t = 0x1;
pub const OVERLAYFS_SUPER_MAGIC: c_long = 0x794c7630;
pub const O_ACCMODE: c_int = 3;
pub const O_APPEND: c_int = 1024;
pub const O_ASYNC: c_int = 0x2000;
pub const O_CLOEXEC: c_int = 0x80000;
pub const O_CREAT: c_int = 64;
pub const O_DIRECT: c_int = 0x4000;
pub const O_DIRECTORY: c_int = 0x10000;
pub const O_DSYNC: c_int = 4096;
pub const O_EXCL: c_int = 128;
pub const O_FSYNC: c_int = 0x101000;
pub const O_LARGEFILE: c_int = 0;
pub const O_NDELAY: c_int = 0x800;
pub const O_NOATIME: c_int = 0o1000000;
pub const O_NOCTTY: c_int = 256;
pub const O_NOFOLLOW: c_int = 0x20000;
pub const O_NONBLOCK: c_int = 2048;
pub const O_PATH: c_int = 0o10000000;
pub const O_RDONLY: c_int = 0;
pub const O_RDWR: c_int = 2;
pub const O_RSYNC: c_int = 1052672;
pub const O_SYNC: c_int = 1052672;
pub const O_TMPFILE: c_int = 4259840;
pub const O_TRUNC: c_int = 512;
pub const O_WRONLY: c_int = 1;
pub const PARENB: tcflag_t = 0x00000100;
pub const PARMRK: tcflag_t = 0x00000008;
pub const PARODD: tcflag_t = 0x00000200;
pub const PATH_MAX: c_int = 4096;
pub const PENDIN: tcflag_t = 0x00004000;
pub const PF_LOCAL: c_int = 1;
pub const PF_ROUTE: c_int = 16;
pub const PIPE_BUF: usize = 4096;
pub const POLLERR: c_short = 0x8;
pub const POLLHUP: c_short = 0x10;
pub const POLLIN: c_short = 0x1;
pub const POLLNVAL: c_short = 0x20;
pub const POLLOUT: c_short = 0x4;
pub const POLLPRI: c_short = 0x2;
pub const POLLRDBAND: c_short = 0x080;
pub const POLLRDNORM: c_short = 0x040;
pub const POLLWRBAND: c_short = 0x200;
pub const POLLWRNORM: c_short = 0x100;
pub const POSIX_FADV_DONTNEED: c_int = 4;
pub const POSIX_FADV_NOREUSE: c_int = 5;
pub const POSIX_FADV_NORMAL: c_int = 0;
pub const POSIX_FADV_RANDOM: c_int = 1;
pub const POSIX_FADV_SEQUENTIAL: c_int = 2;
pub const POSIX_FADV_WILLNEED: c_int = 3;
pub const PRIO_PGRP: __priority_which_t = 1;
pub const PRIO_PROCESS: __priority_which_t = 0;
pub const PRIO_USER: __priority_which_t = 2;
pub const PROC_SUPER_MAGIC: c_long = 0x00009fa0;
pub const PROT_EXEC: c_int = 4;
pub const PROT_GROWSDOWN: c_int = 0x1000000;
pub const PROT_GROWSUP: c_int = 0x2000000;
pub const PROT_NONE: c_int = 0;
pub const PROT_READ: c_int = 1;
pub const PROT_WRITE: c_int = 2;
pub const P_ALL: idtype_t = 0;
pub const P_PGID: idtype_t = 2;
pub const P_PID: idtype_t = 1;
pub const QNX4_SUPER_MAGIC: c_long = 0x0000002f;
pub const QNX6_SUPER_MAGIC: c_long = 0x68191122;
pub const RDTGROUP_SUPER_MAGIC: c_long = 0x7655821;
pub const REISERFS_SUPER_MAGIC: c_long = 0x52654973;
pub const RENAME_EXCHANGE: c_uint = 2;
pub const RENAME_NOREPLACE: c_uint = 1;
pub const RENAME_WHITEOUT: c_uint = 4;
pub const RESOLVE_BENEATH: __u64 = 0x08;
pub const RESOLVE_IN_ROOT: __u64 = 0x10;
pub const RESOLVE_NO_MAGICLINKS: __u64 = 0x02;
pub const RESOLVE_NO_SYMLINKS: __u64 = 0x04;
pub const RESOLVE_NO_XDEV: __u64 = 0x01;
pub const RLIMIT_AS: __rlimit_resource_t = 9;
pub const RLIMIT_CORE: __rlimit_resource_t = 4;
pub const RLIMIT_CPU: __rlimit_resource_t = 0;
pub const RLIMIT_DATA: __rlimit_resource_t = 2;
pub const RLIMIT_FSIZE: __rlimit_resource_t = 1;
pub const RLIMIT_MEMLOCK: __rlimit_resource_t = 8;
pub const RLIMIT_NOFILE: __rlimit_resource_t = 7;
pub const RLIMIT_NPROC: __rlimit_resource_t = 6;
pub const RLIMIT_STACK: __rlimit_resource_t = 3;
pub const RLIM_INFINITY: rlim_t = 18446744073709551615;
pub const RTLD_DEFAULT: *mut c_void = 0x0 as *mut c_void;
pub const RTLD_LAZY: c_int = 1;
pub const R_OK: c_int = 4;
pub const SA_NOCLDSTOP: c_int = 0x00000001;
pub const SA_RESTART: c_int = 0x10000000;
pub const SA_SIGINFO: c_int = 0x00000004;
pub const SECURITYFS_MAGIC: c_long = 0x73636673;
pub const SEEK_CUR: c_int = 1;
pub const SEEK_DATA: c_int = 3;
pub const SEEK_END: c_int = 2;
pub const SEEK_HOLE: c_int = 4;
pub const SEEK_SET: c_int = 0;
pub const SELINUX_MAGIC: c_long = 0xf97cff8c;
pub const SHUT_RD: c_int = 0;
pub const SHUT_RDWR: c_int = 2;
pub const SHUT_WR: c_int = 1;
pub const SIGABRT: c_int = 6;
pub const SIGALRM: c_int = 14;
pub const SIGBUS: c_int = 7;
pub const SIGCHLD: c_int = 17;
pub const SIGCONT: c_int = 18;
pub const SIGFPE: c_int = 8;
pub const SIGHUP: c_int = 1;
pub const SIGILL: c_int = 4;
pub const SIGINT: c_int = 2;
pub const SIGIO: c_int = 29;
pub const SIGKILL: c_int = 9;
pub const SIGPIPE: c_int = 13;
pub const SIGPROF: c_int = 27;
pub const SIGQUIT: c_int = 3;
pub const SIGSEGV: c_int = 11;
pub const SIGSTOP: c_int = 19;
pub const SIGSYS: c_int = 31;
pub const SIGTERM: c_int = 15;
pub const SIGTRAP: c_int = 5;
pub const SIGTSTP: c_int = 20;
pub const SIGTTIN: c_int = 21;
pub const SIGTTOU: c_int = 22;
pub const SIGURG: c_int = 23;
pub const SIGUSR1: c_int = 10;
pub const SIGUSR2: c_int = 12;
pub const SIGVTALRM: c_int = 26;
pub const SIGWINCH: c_int = 28;
pub const SIGXCPU: c_int = 24;
pub const SIGXFSZ: c_int = 25;
pub const SIG_BLOCK: c_int = 0x000000;
pub const SIG_DFL: sighandler_t = 0;
pub const SIG_ERR: sighandler_t = 18446744073709551615;
pub const SIG_IGN: sighandler_t = 1;
pub const SIG_SETMASK: c_int = 2;
pub const SIG_UNBLOCK: c_int = 0x01;
pub const SI_LOAD_SHIFT: c_uint = 16;
pub const SMACK_MAGIC: c_long = 0x43415d53;
pub const SMB_SUPER_MAGIC: c_long = 0x0000517b;
pub const SOCK_CLOEXEC: c_int = 524288;
pub const SOCK_DGRAM: c_int = 2;
pub const SOCK_NONBLOCK: c_int = 2048;
pub const SOCK_RAW: c_int = 3;
pub const SOCK_RDM: c_int = 4;
pub const SOCK_SEQPACKET: c_int = 5;
pub const SOCK_STREAM: c_int = 1;
pub const SOF_TIMESTAMPING_OPT_ID: c_uint = 128;
pub const SOF_TIMESTAMPING_OPT_TSONLY: c_uint = 2048;
pub const SOF_TIMESTAMPING_RAW_HARDWARE: c_uint = 64;
pub const SOF_TIMESTAMPING_RX_HARDWARE: c_uint = 4;
pub const SOF_TIMESTAMPING_RX_SOFTWARE: c_uint = 8;
pub const SOF_TIMESTAMPING_SOFTWARE: c_uint = 16;
pub const SOF_TIMESTAMPING_TX_HARDWARE: c_uint = 1;
pub const SOF_TIMESTAMPING_TX_SOFTWARE: c_uint = 2;
pub const SOL_ALG: c_int = 279;
pub const SOL_IP: c_int = 0;
pub const SOL_IPV6: c_int = 41;
pub const SOL_SOCKET: c_int = 1;
pub const SOL_TCP: c_int = 6;
pub const SOL_TLS: c_int = 282;
pub const SOL_UDP: c_int = 17;
pub const SOMAXCONN: c_int = 4096;
pub const SO_ACCEPTCONN: c_int = 30;
pub const SO_BINDTODEVICE: c_int = 25;
pub const SO_BROADCAST: c_int = 6;
pub const SO_DONTROUTE: c_int = 5;
pub const SO_ERROR: c_int = 4;
pub const SO_KEEPALIVE: c_int = 9;
pub const SO_LINGER: c_int = 13;
pub const SO_MARK: c_int = 36;
pub const SO_OOBINLINE: c_int = 10;
pub const SO_ORIGINAL_DST: c_int = 80;
pub const SO_PASSCRED: c_int = 16;
pub const SO_PEERCRED: c_int = 17;
pub const SO_PEERPIDFD: c_int = 77;
pub const SO_PRIORITY: c_int = 12;
pub const SO_RCVBUF: c_int = 8;
pub const SO_RCVBUFFORCE: c_int = 33;
pub const SO_RCVTIMEO: c_int = 20;
pub const SO_REUSEADDR: c_int = 2;
pub const SO_REUSEPORT: c_int = 15;
pub const SO_RXQ_OVFL: c_int = 40;
pub const SO_SNDBUF: c_int = 7;
pub const SO_SNDBUFFORCE: c_int = 32;
pub const SO_SNDTIMEO: c_int = 21;
pub const SO_TIMESTAMP: c_int = 29;
pub const SO_TIMESTAMPING: c_int = 37;
pub const SO_TIMESTAMPNS: c_int = 35;
pub const SO_TXTIME: c_int = 61;
pub const SO_TYPE: c_int = 3;
pub const STDERR_FILENO: c_int = 2;
pub const STDIN_FILENO: c_int = 0;
pub const STDOUT_FILENO: c_int = 1;
pub const ST_APPEND: c_ulong = 256;
pub const ST_IMMUTABLE: c_ulong = 512;
pub const ST_MANDLOCK: c_ulong = 64;
pub const ST_NOATIME: c_ulong = 1024;
pub const ST_NODEV: c_ulong = 4;
pub const ST_NODIRATIME: c_ulong = 2048;
pub const ST_NOEXEC: c_ulong = 8;
pub const ST_NOSUID: c_ulong = 2;
pub const ST_RDONLY: c_ulong = 1;
pub const ST_RELATIME: c_ulong = 4096;
pub const ST_SYNCHRONOUS: c_ulong = 16;
pub const ST_WRITE: c_ulong = 128;
pub const SYSFS_MAGIC: c_long = 0x62656572;
pub const SYS_futex: c_long = 202;
pub const SYS_getrandom: c_long = 318;
pub const SYS_openat2: c_long = 437;
pub const SYS_pivot_root: c_long = 155;
pub const SYS_renameat2: c_long = 316;
pub const S_IFBLK: mode_t = 0o6_0000;
pub const S_IFCHR: mode_t = 0o2_0000;
pub const S_IFDIR: mode_t = 0o4_0000;
pub const S_IFIFO: mode_t = 0o1_0000;
pub const S_IFLNK: mode_t = 0o12_0000;
pub const S_IFMT: mode_t = 0o17_0000;
pub const S_IFREG: mode_t = 0o10_0000;
pub const S_IFSOCK: mode_t = 0o14_0000;
pub const S_IRGRP: mode_t = 0o0040;
pub const S_IROTH: mode_t = 0o0004;
pub const S_IRUSR: mode_t = 0o0400;
pub const S_IRWXG: mode_t = 0o0070;
pub const S_IRWXO: mode_t = 0o0007;
pub const S_IRWXU: mode_t = 0o0700;
pub const S_ISGID: mode_t = 0o2000;
pub const S_ISUID: mode_t = 0o4000;
pub const S_ISVTX: mode_t = 0o1000;
pub const S_IWGRP: mode_t = 0o0020;
pub const S_IWOTH: mode_t = 0o0002;
pub const S_IWUSR: mode_t = 0o0200;
pub const S_IXGRP: mode_t = 0o0010;
pub const S_IXOTH: mode_t = 0o0001;
pub const S_IXUSR: mode_t = 0o0100;
pub const TAB0: tcflag_t = 0x00000000;
pub const TABDLY: tcflag_t = 0o014000;
pub const TCIFLUSH: c_int = 0;
pub const TCIOFF: c_int = 2;
pub const TCIOFLUSH: c_int = 2;
pub const TCION: c_int = 3;
pub const TCOFLUSH: c_int = 1;
pub const TCOOFF: c_int = 0;
pub const TCOON: c_int = 1;
pub const TCP_CONGESTION: c_int = 13;
pub const TCP_FASTOPEN_CONNECT: c_int = 30;
pub const TCP_KEEPCNT: c_int = 6;
pub const TCP_KEEPIDLE: c_int = 4;
pub const TCP_KEEPINTVL: c_int = 5;
pub const TCP_MAXSEG: c_int = 2;
pub const TCP_NODELAY: c_int = 1;
pub const TCP_REPAIR: c_int = 19;
pub const TCP_ULP: c_int = 31;
pub const TCP_USER_TIMEOUT: c_int = 18;
pub const TCSADRAIN: c_int = 1;
pub const TCSAFLUSH: c_int = 2;
pub const TCSANOW: c_int = 0;
pub const TIOCEXCL: c_ulong = 0x540C;
pub const TIOCGPTPEER: c_ulong = 0x5441;
pub const TIOCGWINSZ: c_ulong = 0x5413;
pub const TIOCNXCL: c_ulong = 0x540D;
pub const TIOCSCTTY: c_ulong = 0x540E;
pub const TIOCSWINSZ: c_ulong = 0x5414;
pub const TLS_RX: c_int = 2;
pub const TLS_TX: c_int = 1;
pub const TMPFS_MAGIC: c_long = 0x01021994;
pub const TOSTOP: tcflag_t = 0x00000100;
pub const TRACEFS_MAGIC: c_long = 0x74726163;
pub const UDF_SUPER_MAGIC: c_long = 0x15013346;
pub const UDP_GRO: c_int = 104;
pub const UDP_SEGMENT: c_int = 103;
pub const USBDEVICE_SUPER_MAGIC: c_long = 0x00009fa2;
pub const UTIME_NOW: c_long = 1073741823;
pub const UTIME_OMIT: c_long = 1073741822;
pub const VDISCARD: usize = 13;
pub const VEOF: usize = 4;
pub const VEOL: usize = 11;
pub const VEOL2: usize = 16;
pub const VERASE: usize = 2;
pub const VINTR: usize = 0;
pub const VKILL: usize = 3;
pub const VLNEXT: usize = 15;
pub const VMIN: usize = 6;
pub const VQUIT: usize = 1;
pub const VREPRINT: usize = 12;
pub const VSTART: usize = 8;
pub const VSTOP: usize = 9;
pub const VSUSP: usize = 10;
pub const VTIME: usize = 5;
pub const VWERASE: usize = 14;
pub const WCONTINUED: c_int = 0x00000008;
pub const WEXITED: c_int = 0x00000004;
pub const WNOHANG: c_int = 0x00000001;
pub const WNOWAIT: c_int = 0x01000000;
pub const WSTOPPED: c_int = 2;
pub const WUNTRACED: c_int = 0x00000002;
pub const W_OK: c_int = 2;
pub const XATTR_CREATE: c_int = 0x1;
pub const XATTR_REPLACE: c_int = 0x2;
pub const XENFS_SUPER_MAGIC: c_long = 0xabba1974;
pub const XFS_SUPER_MAGIC: c_long = 0x58465342;
pub const X_OK: c_int = 1;
pub const _SC_CLK_TCK: c_int = 2;
pub const _SC_PAGESIZE: c_int = 30;

/// PTHREAD_COND_INITIALIZER is a struct-valued initializer whose contents are opaque in
/// libc. Its 48 bytes were read out of the platform ABI by a
/// const-eval probe compiled for x86_64-unknown-linux-gnu and are asserted byte-exact
/// against the oracle.
pub const PTHREAD_COND_INITIALIZER: pthread_cond_t = unsafe {
    ::core::mem::transmute::<[u8; 48], pthread_cond_t>([
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00,
    ])
};

/// PTHREAD_MUTEX_INITIALIZER is a struct-valued initializer whose contents are opaque in
/// libc. Its 40 bytes were read out of the platform ABI by a
/// const-eval probe compiled for x86_64-unknown-linux-gnu and are asserted byte-exact
/// against the oracle.
pub const PTHREAD_MUTEX_INITIALIZER: pthread_mutex_t = unsafe {
    ::core::mem::transmute::<[u8; 40], pthread_mutex_t>([
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ])
};

// ------------------------------------------------------------ functions
unsafe extern "C" {
    pub fn __errno_location() -> *mut c_int;
    pub fn _exit(status: c_int) -> !;
    pub fn abort() -> !;
    pub fn accept(socket: c_int, address: *mut sockaddr, address_len: *mut socklen_t) -> c_int;
    pub fn accept4(fd: c_int, addr: *mut sockaddr, len: *mut socklen_t, flg: c_int) -> c_int;
    pub fn access(path: *const c_char, amode: c_int) -> c_int;
    pub fn bind(socket: c_int, address: *const sockaddr, address_len: socklen_t) -> c_int;
    #[link_name = "cfgetispeed@GLIBC_2.2.5"]
    pub fn cfgetispeed(termios: *const termios) -> speed_t;
    #[link_name = "cfgetospeed@GLIBC_2.2.5"]
    pub fn cfgetospeed(termios: *const termios) -> speed_t;
    pub fn cfmakeraw(termios: *mut termios);
    #[link_name = "cfsetispeed@GLIBC_2.2.5"]
    pub fn cfsetispeed(termios: *mut termios, speed: speed_t) -> c_int;
    #[link_name = "cfsetospeed@GLIBC_2.2.5"]
    pub fn cfsetospeed(termios: *mut termios, speed: speed_t) -> c_int;
    #[link_name = "cfsetspeed@GLIBC_2.2.5"]
    pub fn cfsetspeed(termios: *mut termios, speed: speed_t) -> c_int;
    pub fn chdir(dir: *const c_char) -> c_int;
    pub fn chown(path: *const c_char, uid: uid_t, gid: gid_t) -> c_int;
    pub fn chroot(name: *const c_char) -> c_int;
    pub fn clock_getres(clk_id: clockid_t, tp: *mut timespec) -> c_int;
    pub fn clock_gettime(clk_id: clockid_t, tp: *mut timespec) -> c_int;
    pub fn clock_settime(clk_id: clockid_t, tp: *const timespec) -> c_int;
    pub fn close(fd: c_int) -> c_int;
    pub fn closedir(dirp: *mut DIR) -> c_int;
    pub fn connect(socket: c_int, address: *const sockaddr, len: socklen_t) -> c_int;
    pub fn dirfd(dirp: *mut DIR) -> c_int;
    pub fn dlclose(handle: *mut c_void) -> c_int;
    pub fn dlerror() -> *mut c_char;
    pub fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    pub fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    pub fn dup(fd: c_int) -> c_int;
    pub fn dup2(src: c_int, dst: c_int) -> c_int;
    pub fn dup3(oldfd: c_int, newfd: c_int, flags: c_int) -> c_int;
    pub fn eaccess(pathname: *const c_char, mode: c_int) -> c_int;
    pub fn execv(prog: *const c_char, argv: *const *const c_char) -> c_int;
    pub fn execve(
        prog: *const c_char,
        argv: *const *const c_char,
        envp: *const *const c_char,
    ) -> c_int;
    pub fn execvp(c: *const c_char, argv: *const *const c_char) -> c_int;
    pub fn faccessat(dirfd: c_int, pathname: *const c_char, mode: c_int, flags: c_int) -> c_int;
    pub fn fallocate(fd: c_int, mode: c_int, offset: off_t, len: off_t) -> c_int;
    pub fn fchdir(dirfd: c_int) -> c_int;
    pub fn fchmod(fd: c_int, mode: mode_t) -> c_int;
    pub fn fchmodat(dirfd: c_int, pathname: *const c_char, mode: mode_t, flags: c_int) -> c_int;
    pub fn fchown(fd: c_int, owner: uid_t, group: gid_t) -> c_int;
    pub fn fchownat(
        dirfd: c_int,
        pathname: *const c_char,
        owner: uid_t,
        group: gid_t,
        flags: c_int,
    ) -> c_int;
    pub fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    pub fn fdatasync(fd: c_int) -> c_int;
    pub fn fdopendir(fd: c_int) -> *mut DIR;
    pub fn fgetxattr(
        filedes: c_int,
        name: *const c_char,
        value: *mut c_void,
        size: size_t,
    ) -> ssize_t;
    pub fn fileno(stream: *mut FILE) -> c_int;
    pub fn flistxattr(filedes: c_int, list: *mut c_char, size: size_t) -> ssize_t;
    pub fn flock(fd: c_int, operation: c_int) -> c_int;
    pub fn fork() -> pid_t;
    pub fn forkpty(
        amaster: *mut c_int,
        name: *mut c_char,
        termp: *const termios,
        winp: *const winsize,
    ) -> pid_t;
    pub fn free(p: *mut c_void);
    pub fn freeifaddrs(ifa: *mut ifaddrs);
    pub fn fremovexattr(filedes: c_int, name: *const c_char) -> c_int;
    pub fn fsetxattr(
        filedes: c_int,
        name: *const c_char,
        value: *const c_void,
        size: size_t,
        flags: c_int,
    ) -> c_int;
    pub fn fstat(fildes: c_int, buf: *mut stat) -> c_int;
    pub fn fstatat(dirfd: c_int, pathname: *const c_char, buf: *mut stat, flags: c_int) -> c_int;
    pub fn fstatfs(fd: c_int, buf: *mut statfs) -> c_int;
    pub fn fstatfs64(fd: c_int, buf: *mut statfs64) -> c_int;
    pub fn fstatvfs(fd: c_int, buf: *mut statvfs) -> c_int;
    pub fn fsync(fd: c_int) -> c_int;
    pub fn ftruncate(fd: c_int, length: off_t) -> c_int;
    pub fn futimens(fd: c_int, times: *const timespec) -> c_int;
    pub fn getcwd(buf: *mut c_char, size: size_t) -> *mut c_char;
    pub fn getegid() -> gid_t;
    pub fn getentropy(buf: *mut c_void, buflen: size_t) -> c_int;
    pub fn geteuid() -> uid_t;
    pub fn getgid() -> gid_t;
    pub fn getgrouplist(
        user: *const c_char,
        group: gid_t,
        groups: *mut gid_t,
        ngroups: *mut c_int,
    ) -> c_int;
    pub fn getgroups(ngroups_max: c_int, groups: *mut gid_t) -> c_int;
    pub fn getifaddrs(ifap: *mut *mut ifaddrs) -> c_int;
    pub fn getpeername(socket: c_int, address: *mut sockaddr, address_len: *mut socklen_t)
    -> c_int;
    pub fn getpgid(pid: pid_t) -> pid_t;
    pub fn getpgrp() -> pid_t;
    pub fn getpid() -> pid_t;
    pub fn getppid() -> pid_t;
    pub fn getpriority(which: __priority_which_t, who: id_t) -> c_int;
    pub fn getpwuid_r(
        uid: uid_t,
        pwd: *mut passwd,
        buf: *mut c_char,
        buflen: size_t,
        result: *mut *mut passwd,
    ) -> c_int;
    pub fn getrandom(buf: *mut c_void, buflen: size_t, flags: c_uint) -> ssize_t;
    pub fn getrlimit(resource: __rlimit_resource_t, rlim: *mut rlimit) -> c_int;
    pub fn getsid(pid: pid_t) -> pid_t;
    pub fn getsockname(socket: c_int, address: *mut sockaddr, address_len: *mut socklen_t)
    -> c_int;
    pub fn getsockopt(
        sockfd: c_int,
        level: c_int,
        optname: c_int,
        optval: *mut c_void,
        optlen: *mut socklen_t,
    ) -> c_int;
    pub fn gettimeofday(tp: *mut timeval, tz: *mut timezone) -> c_int;
    pub fn getuid() -> uid_t;
    pub fn getxattr(
        path: *const c_char,
        name: *const c_char,
        value: *mut c_void,
        size: size_t,
    ) -> ssize_t;
    pub fn grantpt(fd: c_int) -> c_int;
    pub fn if_freenameindex(ptr: *mut if_nameindex);
    pub fn if_indextoname(ifindex: c_uint, ifname: *mut c_char) -> *mut c_char;
    pub fn if_nameindex() -> *mut if_nameindex;
    pub fn if_nametoindex(ifname: *const c_char) -> c_uint;
    pub fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    pub fn isatty(fd: c_int) -> c_int;
    pub fn kill(pid: pid_t, sig: c_int) -> c_int;
    pub fn killpg(pgrp: pid_t, sig: c_int) -> c_int;
    pub fn lchown(path: *const c_char, uid: uid_t, gid: gid_t) -> c_int;
    pub fn link(src: *const c_char, dst: *const c_char) -> c_int;
    pub fn linkat(
        olddirfd: c_int,
        oldpath: *const c_char,
        newdirfd: c_int,
        newpath: *const c_char,
        flags: c_int,
    ) -> c_int;
    pub fn listen(socket: c_int, backlog: c_int) -> c_int;
    pub fn listxattr(path: *const c_char, list: *mut c_char, size: size_t) -> ssize_t;
    pub fn login_tty(fd: c_int) -> c_int;
    pub fn lseek(fd: c_int, offset: off_t, whence: c_int) -> off_t;
    pub fn lseek64(fd: c_int, offset: off64_t, whence: c_int) -> off64_t;
    pub fn lstat(path: *const c_char, buf: *mut stat) -> c_int;
    pub fn lutimes(file: *const c_char, times: *const timeval) -> c_int;
    pub fn madvise(addr: *mut c_void, len: size_t, advice: c_int) -> c_int;
    pub fn memfd_create(name: *const c_char, flags: c_uint) -> c_int;
    pub fn mkdir(path: *const c_char, mode: mode_t) -> c_int;
    pub fn mkdirat(dirfd: c_int, pathname: *const c_char, mode: mode_t) -> c_int;
    pub fn mkfifo(path: *const c_char, mode: mode_t) -> c_int;
    pub fn mkfifoat(dirfd: c_int, pathname: *const c_char, mode: mode_t) -> c_int;
    pub fn mknod(pathname: *const c_char, mode: mode_t, dev: dev_t) -> c_int;
    pub fn mknodat(dirfd: c_int, pathname: *const c_char, mode: mode_t, dev: dev_t) -> c_int;
    pub fn mkstemp(template: *mut c_char) -> c_int;
    pub fn mlock(addr: *const c_void, len: size_t) -> c_int;
    pub fn mlockall(flags: c_int) -> c_int;
    pub fn mmap(
        addr: *mut c_void,
        len: size_t,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: off_t,
    ) -> *mut c_void;
    pub fn mmap64(
        addr: *mut c_void,
        len: size_t,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: off64_t,
    ) -> *mut c_void;
    pub fn mprotect(addr: *mut c_void, len: size_t, prot: c_int) -> c_int;
    pub fn mremap(
        addr: *mut c_void,
        len: size_t,
        new_len: size_t,
        flags: c_int,
        ...
    ) -> *mut c_void;
    pub fn msync(addr: *mut c_void, len: size_t, flags: c_int) -> c_int;
    pub fn munlock(addr: *const c_void, len: size_t) -> c_int;
    pub fn munlockall() -> c_int;
    pub fn munmap(addr: *mut c_void, len: size_t) -> c_int;
    pub fn nanosleep(rqtp: *const timespec, rmtp: *mut timespec) -> c_int;
    pub fn nice(incr: c_int) -> c_int;
    pub fn open(path: *const c_char, oflag: c_int, ...) -> c_int;
    pub fn openat(dirfd: c_int, pathname: *const c_char, flags: c_int, ...) -> c_int;
    pub fn openpty(
        amaster: *mut c_int,
        aslave: *mut c_int,
        name: *mut c_char,
        termp: *const termios,
        winp: *const winsize,
    ) -> c_int;
    pub fn pause() -> c_int;
    pub fn pipe(fds: *mut c_int) -> c_int;
    pub fn pipe2(fds: *mut c_int, flags: c_int) -> c_int;
    pub fn poll(fds: *mut pollfd, nfds: nfds_t, timeout: c_int) -> c_int;
    pub fn posix_fadvise(fd: c_int, offset: off_t, len: off_t, advise: c_int) -> c_int;
    pub fn posix_fallocate(fd: c_int, offset: off_t, len: off_t) -> c_int;
    pub fn posix_openpt(flags: c_int) -> c_int;
    pub fn pread(fd: c_int, buf: *mut c_void, count: size_t, offset: off_t) -> ssize_t;
    pub fn pthread_cond_destroy(cond: *mut pthread_cond_t) -> c_int;
    pub fn pthread_cond_init(cond: *mut pthread_cond_t, attr: *const pthread_condattr_t) -> c_int;
    pub fn pthread_cond_signal(cond: *mut pthread_cond_t) -> c_int;
    pub fn pthread_cond_timedwait(
        cond: *mut pthread_cond_t,
        lock: *mut pthread_mutex_t,
        abstime: *const timespec,
    ) -> c_int;
    pub fn pthread_cond_wait(cond: *mut pthread_cond_t, lock: *mut pthread_mutex_t) -> c_int;
    pub fn pthread_condattr_destroy(attr: *mut pthread_condattr_t) -> c_int;
    pub fn pthread_condattr_init(attr: *mut pthread_condattr_t) -> c_int;
    pub fn pthread_condattr_setclock(attr: *mut pthread_condattr_t, clock_id: clockid_t) -> c_int;
    pub fn pthread_kill(thread: pthread_t, sig: c_int) -> c_int;
    pub fn pthread_mutex_destroy(lock: *mut pthread_mutex_t) -> c_int;
    pub fn pthread_mutex_lock(lock: *mut pthread_mutex_t) -> c_int;
    pub fn pthread_mutex_unlock(lock: *mut pthread_mutex_t) -> c_int;
    pub fn pthread_sigmask(how: c_int, set: *const sigset_t, oldset: *mut sigset_t) -> c_int;
    pub fn ptsname(fd: c_int) -> *mut c_char;
    pub fn ptsname_r(fd: c_int, buf: *mut c_char, buflen: size_t) -> c_int;
    pub fn pwrite(fd: c_int, buf: *const c_void, count: size_t, offset: off_t) -> ssize_t;
    pub fn raise(signum: c_int) -> c_int;
    pub fn read(fd: c_int, buf: *mut c_void, count: size_t) -> ssize_t;
    pub fn readdir64(dirp: *mut DIR) -> *mut dirent64;
    pub fn readlink(path: *const c_char, buf: *mut c_char, bufsz: size_t) -> ssize_t;
    pub fn readlinkat(
        dirfd: c_int,
        pathname: *const c_char,
        buf: *mut c_char,
        bufsiz: size_t,
    ) -> ssize_t;
    pub fn readv(fd: c_int, iov: *const iovec, iovcnt: c_int) -> ssize_t;
    pub fn recv(socket: c_int, buf: *mut c_void, len: size_t, flags: c_int) -> ssize_t;
    pub fn recvfrom(
        socket: c_int,
        buf: *mut c_void,
        len: size_t,
        flags: c_int,
        addr: *mut sockaddr,
        addrlen: *mut socklen_t,
    ) -> ssize_t;
    pub fn removexattr(path: *const c_char, name: *const c_char) -> c_int;
    pub fn rename(oldname: *const c_char, newname: *const c_char) -> c_int;
    pub fn renameat(
        olddirfd: c_int,
        oldpath: *const c_char,
        newdirfd: c_int,
        newpath: *const c_char,
    ) -> c_int;
    pub fn renameat2(
        olddirfd: c_int,
        oldpath: *const c_char,
        newdirfd: c_int,
        newpath: *const c_char,
        flags: c_uint,
    ) -> c_int;
    pub fn rewinddir(dirp: *mut DIR);
    pub fn rmdir(path: *const c_char) -> c_int;
    pub fn seekdir(dirp: *mut DIR, loc: c_long);
    pub fn select(
        nfds: c_int,
        readfds: *mut fd_set,
        writefds: *mut fd_set,
        errorfds: *mut fd_set,
        timeout: *mut timeval,
    ) -> c_int;
    pub fn send(socket: c_int, buf: *const c_void, len: size_t, flags: c_int) -> ssize_t;
    pub fn sendmsg(fd: c_int, msg: *const msghdr, flags: c_int) -> ssize_t;
    pub fn sendto(
        socket: c_int,
        buf: *const c_void,
        len: size_t,
        flags: c_int,
        addr: *const sockaddr,
        addrlen: socklen_t,
    ) -> ssize_t;
    pub fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
    pub fn setpgid(pid: pid_t, pgid: pid_t) -> c_int;
    pub fn setpriority(which: __priority_which_t, who: id_t, prio: c_int) -> c_int;
    pub fn setrlimit(resource: __rlimit_resource_t, rlim: *const rlimit) -> c_int;
    pub fn setsid() -> pid_t;
    pub fn setsockopt(
        socket: c_int,
        level: c_int,
        name: c_int,
        value: *const c_void,
        option_len: socklen_t,
    ) -> c_int;
    pub fn shm_open(name: *const c_char, oflag: c_int, mode: mode_t) -> c_int;
    pub fn shm_unlink(name: *const c_char) -> c_int;
    pub fn shutdown(socket: c_int, how: c_int) -> c_int;
    pub fn sigaction(signum: c_int, act: *const sigaction, oldact: *mut sigaction) -> c_int;
    pub fn sigaddset(set: *mut sigset_t, signum: c_int) -> c_int;
    pub fn sigemptyset(set: *mut sigset_t) -> c_int;
    pub fn signal(signum: c_int, handler: sighandler_t) -> sighandler_t;
    pub fn sigprocmask(how: c_int, set: *const sigset_t, oldset: *mut sigset_t) -> c_int;
    pub fn sigwait(set: *const sigset_t, sig: *mut c_int) -> c_int;
    pub fn sleep(secs: c_uint) -> c_uint;
    pub fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int;
    pub fn socketpair(
        domain: c_int,
        type_: c_int,
        protocol: c_int,
        socket_vector: *mut c_int,
    ) -> c_int;
    pub fn stat(path: *const c_char, buf: *mut stat) -> c_int;
    pub fn statfs(path: *const c_char, buf: *mut statfs) -> c_int;
    pub fn statfs64(path: *const c_char, buf: *mut statfs64) -> c_int;
    pub fn statvfs(path: *const c_char, buf: *mut statvfs) -> c_int;
    #[link_name = "__xpg_strerror_r"]
    pub fn strerror_r(errnum: c_int, buf: *mut c_char, buflen: size_t) -> c_int;
    pub fn strlen(cs: *const c_char) -> size_t;
    pub fn symlink(path1: *const c_char, path2: *const c_char) -> c_int;
    pub fn symlinkat(target: *const c_char, newdirfd: c_int, linkpath: *const c_char) -> c_int;
    pub fn sync();
    pub fn syncfs(fd: c_int) -> c_int;
    pub fn syscall(num: c_long, ...) -> c_long;
    pub fn sysconf(name: c_int) -> c_long;
    pub fn sysctl(
        name: *mut c_int,
        namelen: c_int,
        oldp: *mut c_void,
        oldlenp: *mut size_t,
        newp: *mut c_void,
        newlen: size_t,
    ) -> c_int;
    pub fn sysinfo(info: *mut sysinfo) -> c_int;
    pub fn tcdrain(fd: c_int) -> c_int;
    pub fn tcflow(fd: c_int, action: c_int) -> c_int;
    pub fn tcflush(fd: c_int, action: c_int) -> c_int;
    pub fn tcgetattr(fd: c_int, termios: *mut termios) -> c_int;
    pub fn tcgetpgrp(fd: c_int) -> pid_t;
    pub fn tcgetsid(fd: c_int) -> pid_t;
    pub fn tcsendbreak(fd: c_int, duration: c_int) -> c_int;
    pub fn tcsetattr(fd: c_int, optional_actions: c_int, termios: *const termios) -> c_int;
    pub fn tcsetpgrp(fd: c_int, pgrp: pid_t) -> c_int;
    pub fn truncate(path: *const c_char, length: off_t) -> c_int;
    pub fn umask(mask: mode_t) -> mode_t;
    pub fn unlink(c: *const c_char) -> c_int;
    pub fn unlinkat(dirfd: c_int, pathname: *const c_char, flags: c_int) -> c_int;
    pub fn unlockpt(fd: c_int) -> c_int;
    pub fn usleep(secs: c_uint) -> c_int;
    pub fn utimensat(
        dirfd: c_int,
        path: *const c_char,
        times: *const timespec,
        flag: c_int,
    ) -> c_int;
    pub fn utimes(filename: *const c_char, times: *const timeval) -> c_int;
    pub fn waitid(idtype: idtype_t, id: id_t, infop: *mut siginfo_t, options: c_int) -> c_int;
    pub fn waitpid(pid: pid_t, status: *mut c_int, options: c_int) -> pid_t;
    pub fn write(fd: c_int, buf: *const c_void, count: size_t) -> ssize_t;
    pub fn writev(fd: c_int, iov: *const iovec, iovcnt: c_int) -> ssize_t;
}

// ------------------------------------------------------------------------
// The C macros. These and the siginfo_t accessors below are the two
// hand-written body classes: real code with bit-exact semantics for which
// declaration and signature equality alone are not enough.
// The oracle covers every body: status helpers over -1..=0xFFFF plus
// signed/high-bit extremes, device helpers over deterministic samples,
// and pointer-writing/ancillary-data helpers with native runtime tests.
// ------------------------------------------------------------------------
/// The word `fd_set` is a bitmap of. Selecting it from the measured
/// alignment rather than naming a C type keeps the bit order pinned to
/// the same layout the oracle diffs byte-for-byte.
type fd_word = u64;

/// Control messages are padded out to a pointer-sized boundary.
const fn cmsg_align(len: usize) -> usize {
    (len + ::core::mem::size_of::<usize>() - 1) & !(::core::mem::size_of::<usize>() - 1)
}

/// # Safety
///
/// `mhdr` must point to a valid `msghdr` whose `msg_control` /
/// `msg_controllen` describe one allocation.
#[inline]
pub unsafe extern "C" fn CMSG_FIRSTHDR(mhdr: *const msghdr) -> *mut cmsghdr {
    unsafe {
        if (*mhdr).msg_controllen as usize >= ::core::mem::size_of::<cmsghdr>() {
            (*mhdr).msg_control.cast::<cmsghdr>()
        } else {
            ::core::ptr::null_mut::<cmsghdr>()
        }
    }
}

/// # Safety
///
/// Always sound: this is arithmetic only. It is `unsafe` because
/// upstream declares it so, and the signature is part of the contract.
#[inline]
pub const unsafe extern "C" fn CMSG_LEN(length: c_uint) -> c_uint {
    cmsg_align(::core::mem::size_of::<cmsghdr>()) as c_uint + length
}

/// # Safety
///
/// Always sound: this is arithmetic only. It is `unsafe` because
/// upstream declares it so, and the signature is part of the contract.
#[inline]
pub const unsafe extern "C" fn CMSG_SPACE(length: c_uint) -> c_uint {
    (cmsg_align(length as usize) + cmsg_align(::core::mem::size_of::<cmsghdr>())) as c_uint
}

/// # Safety
///
/// `set` must point to a valid, initialised `fd_set`, and `fd` must be in
/// `0..FD_SETSIZE` — out of range reads past the end of the bitmap.
#[inline]
pub unsafe extern "C" fn FD_ISSET(fd: c_int, set: *const fd_set) -> bool {
    let bits = ::core::mem::size_of::<fd_word>() * 8;
    let fd = fd as usize;
    ((unsafe { (*set).__opaque[fd / bits] }) & (1 << (fd % bits))) != 0
}

/// # Safety
///
/// `set` must point to a valid, writable `fd_set`, and `fd` must be in
/// `0..FD_SETSIZE` — out of range indexes past the end of the bitmap.
#[inline]
pub unsafe extern "C" fn FD_SET(fd: c_int, set: *mut fd_set) {
    let bits = ::core::mem::size_of::<fd_word>() * 8;
    let fd = fd as usize;
    unsafe { (*set).__opaque[fd / bits] |= 1 << (fd % bits) };
}

/// # Safety
///
/// `set` must point to a valid, writable `fd_set`.
#[inline]
pub unsafe extern "C" fn FD_ZERO(set: *mut fd_set) {
    unsafe { (*set).__opaque.fill(0) }
}

#[inline]
pub const extern "C" fn WEXITSTATUS(status: c_int) -> c_int {
    (status >> 8) & 0xff
}

#[inline]
pub const extern "C" fn WIFCONTINUED(status: c_int) -> bool {
    status == 0xffff
}

#[inline]
pub const extern "C" fn WIFEXITED(status: c_int) -> bool {
    (status & 0x7f) == 0
}

#[inline]
pub const extern "C" fn WIFSIGNALED(status: c_int) -> bool {
    ((status & 0x7f) + 1) as i8 >= 2
}

#[inline]
pub const extern "C" fn WIFSTOPPED(status: c_int) -> bool {
    (status & 0xff) == 0x7f
}

#[inline]
pub const extern "C" fn WSTOPSIG(status: c_int) -> c_int {
    (status >> 8) & 0xff
}

#[inline]
pub const extern "C" fn WTERMSIG(status: c_int) -> c_int {
    status & 0x7f
}

#[inline]
pub const extern "C" fn major(dev: dev_t) -> c_uint {
    let mut major = 0;
    major |= (dev & 0x0000_0000_000f_ff00) >> 8;
    major |= (dev & 0xffff_f000_0000_0000) >> 32;
    major as c_uint
}

#[inline]
pub const extern "C" fn makedev(major: c_uint, minor: c_uint) -> dev_t {
    let major = major as dev_t;
    let minor = minor as dev_t;
    let mut dev = 0;
    dev |= (major & 0x0000_0fff) << 8;
    dev |= (major & 0xffff_f000) << 32;
    dev |= (minor & 0x0000_00ff) << 0;
    dev |= (minor & 0xffff_ff00) << 12;
    dev
}

#[inline]
pub const extern "C" fn minor(dev: dev_t) -> c_uint {
    let mut minor = 0;
    minor |= (dev & 0x0000_0000_0000_00ff) >> 0;
    minor |= (dev & 0x0000_0fff_fff0_0000) >> 12;
    minor as c_uint
}

// ------------------------------------------------------------------------
// Inherent methods libc puts on these types. Reinterpret-casts over the
// private payload, each carrying its own repr(C) prefix struct.
// ------------------------------------------------------------------------
impl siginfo_t {
    /// # Safety
    ///
    /// The caller must know the signal was one that sets `si_addr`
    /// (SIGSEGV, SIGBUS, SIGILL, SIGFPE); the field is a union arm and reading
    /// the wrong arm reads uninitialised bytes.
    pub unsafe fn si_addr(&self) -> *mut c_void {
        #[repr(C)]
        struct siginfo_sigfault {
            _si_signo: c_int,
            _si_errno: c_int,
            _si_code: c_int,
            si_addr: *mut c_void,
        }
        unsafe { (*(self as *const siginfo_t).cast::<siginfo_sigfault>()).si_addr }
    }

    /// # Safety
    ///
    /// The caller must know the signal was queued with a value
    /// (`sigqueue`, or a POSIX timer); this reads a union arm.
    pub unsafe fn si_value(&self) -> sigval {
        #[repr(C)]
        struct siginfo_timer {
            _si_signo: c_int,
            _si_errno: c_int,
            _si_code: c_int,
            _si_tid: c_int,
            _si_overrun: c_int,
            si_sigval: sigval,
        }
        unsafe { (*(self as *const siginfo_t).cast::<siginfo_timer>()).si_sigval }
    }

    /// # Safety
    ///
    /// The caller must know the signal was a SIGCHLD; this reads a union arm.
    pub unsafe fn si_pid(&self) -> pid_t {
        unsafe { self.sigchld().si_pid }
    }

    /// # Safety
    ///
    /// The caller must know the signal was a SIGCHLD; this reads a union arm.
    pub unsafe fn si_uid(&self) -> uid_t {
        unsafe { self.sigchld().si_uid }
    }

    /// # Safety
    ///
    /// The caller must know the signal was a SIGCHLD; this reads a union arm.
    pub unsafe fn si_status(&self) -> c_int {
        unsafe { self.sigchld().si_status }
    }

    /// # Safety
    ///
    /// The caller must know the signal was a SIGCHLD; this reads a union arm.
    pub unsafe fn si_utime(&self) -> c_long {
        unsafe { self.sigchld().si_utime }
    }

    /// # Safety
    ///
    /// The caller must know the signal was a SIGCHLD; this reads a union arm.
    pub unsafe fn si_stime(&self) -> c_long {
        unsafe { self.sigchld().si_stime }
    }

    /// The `sigchld` arm of the private `_sifields` union. Note the pointer in
    /// the alignment slot: some arms of that union start with one, which is
    /// what fixes where the arm begins.
    unsafe fn sigchld(&self) -> &sifields_sigchld {
        #[repr(C)]
        struct sifields_sigchld_arm {
            _align_pointer: *mut c_void,
        }
        #[repr(C)]
        struct siginfo_f {
            _siginfo_base: [c_int; 3],
            _align: sifields_sigchld_arm,
        }
        let base = (self as *const siginfo_t).cast::<siginfo_f>();
        unsafe { &*(&raw const (*base)._align).cast::<sifields_sigchld>() }
    }
}

#[repr(C)]
struct sifields_sigchld {
    si_pid: pid_t,
    si_uid: uid_t,
    si_status: c_int,
    si_utime: c_long,
    si_stime: c_long,
}
