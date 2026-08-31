// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `libc` for `aarch64-apple-darwin`.
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
pub type blkcnt_t = i64;
pub type blksize_t = i32;
pub type cc_t = c_uchar;
pub type clockid_t = c_uint;
pub type dev_t = i32;
pub type fsblkcnt_t = c_uint;
pub type fsfilcnt_t = c_uint;
pub type gid_t = u32;
pub type host_flavor_t = integer_t;
pub type host_info64_t = *mut integer_t;
pub type host_t = c_uint;
pub type id_t = c_uint;
pub type idtype_t = c_uint;
pub type in_addr_t = u32;
pub type in_port_t = u16;
pub type ino_t = u64;
pub type integer_t = c_int;
pub type intptr_t = isize;
pub type kern_return_t = c_int;
pub type mach_msg_type_number_t = natural_t;
pub type mode_t = u16;
pub type natural_t = u32;
pub type nfds_t = c_uint;
pub type nlink_t = u16;
pub type off_t = i64;
pub type pid_t = i32;
pub type pthread_t = uintptr_t;
pub type rlim_t = u64;
pub type sa_family_t = u8;
pub type sighandler_t = size_t;
pub type sigset_t = u32;
pub type size_t = usize;
pub type socklen_t = u32;
pub type speed_t = c_ulong;
pub type ssize_t = isize;
pub type suseconds_t = i32;
pub type tcflag_t = c_ulong;
pub type time_t = c_long;
pub type uid_t = u32;
pub type uintptr_t = usize;
pub type vm_statistics64_t = *mut vm_statistics64;

/// Opaque to callers: libc exposes this as an uninhabited type.
#[derive(Clone, Copy, Debug)]
pub enum DIR {}

/// Opaque to callers: libc exposes this as an uninhabited type.
#[derive(Clone, Copy, Debug)]
pub enum FILE {}

#[repr(u32)]
#[derive(Clone, Copy, Debug)]
pub enum qos_class_t {
    QOS_CLASS_USER_INTERACTIVE = 0x21,
    QOS_CLASS_USER_INITIATED = 0x19,
    QOS_CLASS_DEFAULT = 0x15,
    QOS_CLASS_UTILITY = 0x11,
    QOS_CLASS_BACKGROUND = 0x09,
    QOS_CLASS_UNSPECIFIED = 0x00,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct dirent {
    pub d_ino: u64,
    pub d_seekoff: u64,
    pub d_reclen: u16,
    pub d_namlen: u16,
    pub d_type: u8,
    pub d_name: [c_char; 1024],
}

/// opaque in libc: 128 bytes, align 4
#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct fd_set {
    __opaque: [u32; 32],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct flock {
    pub l_start: off_t,
    pub l_len: off_t,
    pub l_pid: pid_t,
    pub l_type: c_short,
    pub l_whence: c_short,
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
pub struct fstore_t {
    pub fst_flags: c_uint,
    pub fst_posmode: c_int,
    pub fst_offset: off_t,
    pub fst_length: off_t,
    pub fst_bytesalloc: off_t,
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
    pub ifa_dstaddr: *mut sockaddr,
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

#[repr(C, packed(4))]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct kevent {
    pub ident: uintptr_t,
    pub filter: i16,
    pub flags: u16,
    pub fflags: u32,
    pub data: intptr_t,
    pub udata: *mut c_void,
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
pub struct passwd {
    pub pw_name: *mut c_char,
    pub pw_passwd: *mut c_char,
    pub pw_uid: uid_t,
    pub pw_gid: gid_t,
    pub pw_change: time_t,
    pub pw_class: *mut c_char,
    pub pw_gecos: *mut c_char,
    pub pw_dir: *mut c_char,
    pub pw_shell: *mut c_char,
    pub pw_expire: time_t,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct pollfd {
    pub fd: c_int,
    pub events: c_short,
    pub revents: c_short,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct proc_bsdinfo {
    pub pbi_flags: u32,
    pub pbi_status: u32,
    pub pbi_xstatus: u32,
    pub pbi_pid: u32,
    pub pbi_ppid: u32,
    pub pbi_uid: uid_t,
    pub pbi_gid: gid_t,
    pub pbi_ruid: uid_t,
    pub pbi_rgid: gid_t,
    pub pbi_svuid: uid_t,
    pub pbi_svgid: gid_t,
    pub rfu_1: u32,
    pub pbi_comm: [c_char; 16],
    pub pbi_name: [c_char; 32],
    pub pbi_nfiles: u32,
    pub pbi_pgid: u32,
    pub pbi_pjobc: u32,
    pub e_tdev: u32,
    pub e_tpgid: u32,
    pub pbi_nice: i32,
    pub pbi_start_tvsec: u64,
    pub pbi_start_tvusec: u64,
}

/// opaque in libc: 48 bytes, align 8
#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct pthread_cond_t {
    __opaque: [u64; 6],
}

/// opaque in libc: 16 bytes, align 8
#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct pthread_condattr_t {
    __opaque: [u64; 2],
}

/// opaque in libc: 64 bytes, align 8
#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct pthread_mutex_t {
    __opaque: [u64; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct radvisory {
    pub ra_offset: off_t,
    pub ra_count: c_int,
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
pub struct sigaction {
    pub sa_sigaction: sighandler_t,
    pub sa_mask: sigset_t,
    pub sa_flags: c_int,
}

/// libc keeps private fields here; the __pad members reproduce their measured extent (total 104 bytes, align 8)
#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct siginfo_t {
    pub si_signo: c_int,
    pub si_errno: c_int,
    pub si_code: c_int,
    pub si_pid: pid_t,
    pub si_uid: uid_t,
    pub si_status: c_int,
    pub si_addr: *mut c_void,
    __pad0: [u8; 72],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sigval {
    pub sival_ptr: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sockaddr {
    pub sa_len: u8,
    pub sa_family: sa_family_t,
    pub sa_data: [c_char; 14],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sockaddr_dl {
    pub sdl_len: c_uchar,
    pub sdl_family: c_uchar,
    pub sdl_index: c_ushort,
    pub sdl_type: c_uchar,
    pub sdl_nlen: c_uchar,
    pub sdl_alen: c_uchar,
    pub sdl_slen: c_uchar,
    pub sdl_data: [c_char; 12],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sockaddr_in {
    pub sin_len: u8,
    pub sin_family: sa_family_t,
    pub sin_port: in_port_t,
    pub sin_addr: in_addr,
    pub sin_zero: [c_char; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sockaddr_in6 {
    pub sin6_len: u8,
    pub sin6_family: sa_family_t,
    pub sin6_port: in_port_t,
    pub sin6_flowinfo: u32,
    pub sin6_addr: in6_addr,
    pub sin6_scope_id: u32,
}

/// libc keeps private fields here; the __pad members reproduce their measured extent (total 128 bytes, align 8)
/// alignment stated explicitly: libc gets it from a private field whose type is not part of the contract
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sockaddr_storage {
    pub ss_len: u8,
    pub ss_family: sa_family_t,
    __pad0: [u8; 126],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sockaddr_un {
    pub sun_len: u8,
    pub sun_family: sa_family_t,
    pub sun_path: [c_char; 104],
}

#[repr(C, packed(1))]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct sockaddr_vm {
    pub svm_len: c_uchar,
    pub svm_family: sa_family_t,
    pub svm_reserved1: c_ushort,
    pub svm_port: c_uint,
    pub svm_cid: c_uint,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct stat {
    pub st_dev: dev_t,
    pub st_mode: mode_t,
    pub st_nlink: nlink_t,
    pub st_ino: ino_t,
    pub st_uid: uid_t,
    pub st_gid: gid_t,
    pub st_rdev: dev_t,
    pub st_atime: time_t,
    pub st_atime_nsec: c_long,
    pub st_mtime: time_t,
    pub st_mtime_nsec: c_long,
    pub st_ctime: time_t,
    pub st_ctime_nsec: c_long,
    pub st_birthtime: time_t,
    pub st_birthtime_nsec: c_long,
    pub st_size: off_t,
    pub st_blocks: blkcnt_t,
    pub st_blksize: blksize_t,
    pub st_flags: u32,
    pub st_gen: u32,
    pub st_lspare: i32,
    pub st_qspare: [i64; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct statfs {
    pub f_bsize: u32,
    pub f_iosize: i32,
    pub f_blocks: u64,
    pub f_bfree: u64,
    pub f_bavail: u64,
    pub f_files: u64,
    pub f_ffree: u64,
    pub f_fsid: fsid_t,
    pub f_owner: uid_t,
    pub f_type: u32,
    pub f_flags: u32,
    pub f_fssubtype: u32,
    pub f_fstypename: [c_char; 16],
    pub f_mntonname: [c_char; 1024],
    pub f_mntfromname: [c_char; 1024],
    pub f_flags_ext: u32,
    pub f_reserved: [u32; 7],
}

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
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct termios {
    pub c_iflag: tcflag_t,
    pub c_oflag: tcflag_t,
    pub c_cflag: tcflag_t,
    pub c_lflag: tcflag_t,
    pub c_cc: [cc_t; 20],
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

/// UPSTREAM-PARITY DIVERGENCE FROM THE PLATFORM SDK, inherited and deliberate.
///
/// libc 0.2.186 declares this struct at Mach **rev1** -- the 24 fields and 152
/// bytes measured here. The SDK on the machine that generated this file
/// (`MacOSX.sdk/usr/include/mach/vm_statistics.h`) is at **rev3**: 36 fields and
/// 248 bytes, rev2 having added `swapped_count` and rev3 eleven more counters.
/// This crate reproduces UPSTREAM byte-for-byte, so the oracle is green on it and
/// the crates.io `libc` it replaces would behave identically.
///
/// That parity is the decision, not an oversight. Nothing in aterm's graph calls
/// `host_statistics64`: the only reference to it anywhere in the build is rustix's
/// `pub(super) use host_statistics64 as host_statistics, vm_statistics64_t as
/// vm_statistics_t` in `src/backend/libc/c.rs` -- present in both 0.38.44 and
/// 1.1.4, called by neither, and a `use` of a name that must therefore exist,
/// which is the whole reason this struct is declared at all. Measured by grepping
/// every one of the 587 packages in `Cargo.lock`. No caller can observe the
/// difference, and for a drop-in replacement, differing from the crate it replaces
/// is the worse property.
///
/// If a caller ever does appear, this layout is NOT the one to hand
/// `host_statistics64` alongside an SDK-derived `HOST_VM_INFO64_COUNT` (62 words):
/// the kernel would write 248 bytes into 152. At that point the SDK is right and
/// upstream is the bug -- widen it here deliberately, and state the divergence
/// where the oracle can see it rather than leaving it silent.
#[repr(C, packed(8))]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct vm_statistics64 {
    pub free_count: natural_t,
    pub active_count: natural_t,
    pub inactive_count: natural_t,
    pub wire_count: natural_t,
    pub zero_fill_count: u64,
    pub reactivations: u64,
    pub pageins: u64,
    pub pageouts: u64,
    pub faults: u64,
    pub cow_faults: u64,
    pub lookups: u64,
    pub hits: u64,
    pub purges: u64,
    pub purgeable_count: natural_t,
    pub speculative_count: natural_t,
    pub decompressions: u64,
    pub compressions: u64,
    pub swapins: u64,
    pub swapouts: u64,
    pub compressor_page_count: natural_t,
    pub throttled_count: natural_t,
    pub external_page_count: natural_t,
    pub internal_page_count: natural_t,
    pub total_uncompressed_pages_in_compressor: u64,
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

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "extra_traits", derive(PartialEq, Eq, Hash))]
pub struct xucred {
    pub cr_version: c_uint,
    pub cr_uid: uid_t,
    pub cr_ngroups: c_short,
    pub cr_groups: [gid_t; 16],
}

// ------------------------------------------------------------ constants
pub const AF_APPLETALK: c_int = 16;
pub const AF_CCITT: c_int = 10;
pub const AF_CHAOS: c_int = 5;
pub const AF_CNT: c_int = 21;
pub const AF_COIP: c_int = 20;
pub const AF_DATAKIT: c_int = 9;
pub const AF_DECnet: c_int = 12;
pub const AF_DLI: c_int = 13;
pub const AF_HYLINK: c_int = 15;
pub const AF_IMPLINK: c_int = 3;
pub const AF_INET: c_int = 2;
pub const AF_INET6: c_int = 30;
pub const AF_IPX: c_int = 23;
pub const AF_ISDN: c_int = 28;
pub const AF_ISO: c_int = 7;
pub const AF_LAT: c_int = 14;
pub const AF_LINK: c_int = 18;
pub const AF_NATM: c_int = 31;
pub const AF_NS: c_int = 6;
pub const AF_PUP: c_int = 4;
pub const AF_SNA: c_int = 11;
pub const AF_SYSTEM: c_int = 32;
pub const AF_UNIX: c_int = 1;
pub const AF_UNSPEC: c_int = 0;
pub const AF_VSOCK: c_int = 40;
pub const AT_EACCESS: c_int = 0x0010;
pub const AT_FDCWD: c_int = -2;
pub const AT_REMOVEDIR: c_int = 0x0080;
pub const AT_SYMLINK_FOLLOW: c_int = 0x0040;
pub const AT_SYMLINK_NOFOLLOW: c_int = 0x0020;
pub const B230400: speed_t = 230400;
pub const BRKINT: tcflag_t = 0x00000002;
pub const CLD_CONTINUED: c_int = 6;
pub const CLD_DUMPED: c_int = 3;
pub const CLD_EXITED: c_int = 1;
pub const CLD_KILLED: c_int = 2;
pub const CLD_STOPPED: c_int = 5;
pub const CLD_TRAPPED: c_int = 4;
pub const CLOCAL: tcflag_t = 0x00008000;
pub const CLOCK_MONOTONIC: clockid_t = 6;
pub const CLOCK_PROCESS_CPUTIME_ID: clockid_t = 12;
pub const CLOCK_REALTIME: clockid_t = 0;
pub const CLOCK_THREAD_CPUTIME_ID: clockid_t = 16;
pub const CREAD: tcflag_t = 0x00000800;
pub const CRTSCTS: tcflag_t = 0x00030000;
pub const CS5: tcflag_t = 0x00000000;
pub const CS6: tcflag_t = 0x00000100;
pub const CS7: tcflag_t = 0x00000200;
pub const CS8: tcflag_t = 0x00000300;
pub const CSIZE: tcflag_t = 0x00000300;
pub const CSTOPB: tcflag_t = 0x00000400;
pub const CTL_KERN: c_int = 1;
pub const DT_BLK: u8 = 6;
pub const DT_CHR: u8 = 2;
pub const DT_DIR: u8 = 4;
pub const DT_FIFO: u8 = 1;
pub const DT_LNK: u8 = 10;
pub const DT_REG: u8 = 8;
pub const DT_SOCK: u8 = 12;
pub const E2BIG: c_int = 7;
pub const EACCES: c_int = 13;
pub const EADDRINUSE: c_int = 48;
pub const EADDRNOTAVAIL: c_int = 49;
pub const EAFNOSUPPORT: c_int = 47;
pub const EAGAIN: c_int = 35;
pub const EALREADY: c_int = 37;
pub const EAUTH: c_int = 80;
pub const EBADARCH: c_int = 86;
pub const EBADEXEC: c_int = 85;
pub const EBADF: c_int = 9;
pub const EBADMACHO: c_int = 88;
pub const EBADMSG: c_int = 94;
pub const EBADRPC: c_int = 72;
pub const EBUSY: c_int = 16;
pub const ECANCELED: c_int = 89;
pub const ECHILD: c_int = 10;
pub const ECHO: tcflag_t = 0x00000008;
pub const ECHOCTL: tcflag_t = 0x00000040;
pub const ECHOE: tcflag_t = 0x00000002;
pub const ECHOK: tcflag_t = 0x00000004;
pub const ECHOKE: tcflag_t = 0x00000001;
pub const ECHONL: tcflag_t = 0x00000010;
pub const ECHOPRT: tcflag_t = 0x00000020;
pub const ECONNABORTED: c_int = 53;
pub const ECONNREFUSED: c_int = 61;
pub const ECONNRESET: c_int = 54;
pub const EDEADLK: c_int = 11;
pub const EDESTADDRREQ: c_int = 39;
pub const EDEVERR: c_int = 83;
pub const EDOM: c_int = 33;
pub const EDQUOT: c_int = 69;
pub const EEXIST: c_int = 17;
pub const EFAULT: c_int = 14;
pub const EFBIG: c_int = 27;
pub const EFTYPE: c_int = 79;
pub const EHOSTDOWN: c_int = 64;
pub const EHOSTUNREACH: c_int = 65;
pub const EIDRM: c_int = 90;
pub const EILSEQ: c_int = 92;
pub const EINPROGRESS: c_int = 36;
pub const EINTR: c_int = 4;
pub const EINVAL: c_int = 22;
pub const EIO: c_int = 5;
pub const EISCONN: c_int = 56;
pub const EISDIR: c_int = 21;
pub const ELOOP: c_int = 62;
pub const EMFILE: c_int = 24;
pub const EMLINK: c_int = 31;
pub const EMSGSIZE: c_int = 40;
pub const EMULTIHOP: c_int = 95;
pub const ENAMETOOLONG: c_int = 63;
pub const ENEEDAUTH: c_int = 81;
pub const ENETDOWN: c_int = 50;
pub const ENETRESET: c_int = 52;
pub const ENETUNREACH: c_int = 51;
pub const ENFILE: c_int = 23;
pub const ENOATTR: c_int = 93;
pub const ENOBUFS: c_int = 55;
pub const ENODATA: c_int = 96;
pub const ENODEV: c_int = 19;
pub const ENOENT: c_int = 2;
pub const ENOEXEC: c_int = 8;
pub const ENOLCK: c_int = 77;
pub const ENOLINK: c_int = 97;
pub const ENOMEM: c_int = 12;
pub const ENOMSG: c_int = 91;
pub const ENOPOLICY: c_int = 103;
pub const ENOPROTOOPT: c_int = 42;
pub const ENOSPC: c_int = 28;
pub const ENOSR: c_int = 98;
pub const ENOSTR: c_int = 99;
pub const ENOSYS: c_int = 78;
pub const ENOTBLK: c_int = 15;
pub const ENOTCONN: c_int = 57;
pub const ENOTDIR: c_int = 20;
pub const ENOTEMPTY: c_int = 66;
pub const ENOTRECOVERABLE: c_int = 104;
pub const ENOTSOCK: c_int = 38;
pub const ENOTSUP: c_int = 45;
pub const ENOTTY: c_int = 25;
pub const ENXIO: c_int = 6;
pub const EOPNOTSUPP: c_int = 102;
pub const EOVERFLOW: c_int = 84;
pub const EOWNERDEAD: c_int = 105;
pub const EPERM: c_int = 1;
pub const EPFNOSUPPORT: c_int = 46;
pub const EPIPE: c_int = 32;
pub const EPROCLIM: c_int = 67;
pub const EPROCUNAVAIL: c_int = 76;
pub const EPROGMISMATCH: c_int = 75;
pub const EPROGUNAVAIL: c_int = 74;
pub const EPROTO: c_int = 100;
pub const EPROTONOSUPPORT: c_int = 43;
pub const EPROTOTYPE: c_int = 41;
pub const EPWROFF: c_int = 82;
pub const EQFULL: c_int = 106;
pub const ERANGE: c_int = 34;
pub const EREMOTE: c_int = 71;
pub const EROFS: c_int = 30;
pub const ERPCMISMATCH: c_int = 73;
pub const ESHLIBVERS: c_int = 87;
pub const ESHUTDOWN: c_int = 58;
pub const ESOCKTNOSUPPORT: c_int = 44;
pub const ESPIPE: c_int = 29;
pub const ESRCH: c_int = 3;
pub const ESTALE: c_int = 70;
pub const ETIME: c_int = 101;
pub const ETIMEDOUT: c_int = 60;
pub const ETOOMANYREFS: c_int = 59;
pub const ETXTBSY: c_int = 26;
pub const EUSERS: c_int = 68;
pub const EVFILT_PROC: i16 = -5;
pub const EVFILT_READ: i16 = -1;
pub const EVFILT_SIGNAL: i16 = -6;
pub const EVFILT_TIMER: i16 = -7;
pub const EVFILT_USER: i16 = -10;
pub const EVFILT_VNODE: i16 = -4;
pub const EVFILT_WRITE: i16 = -2;
pub const EV_ADD: u16 = 0x1;
pub const EV_CLEAR: u16 = 0x20;
pub const EV_DELETE: u16 = 0x2;
pub const EV_DISABLE: u16 = 0x8;
pub const EV_ENABLE: u16 = 0x4;
pub const EV_EOF: u16 = 0x8000;
pub const EV_ERROR: u16 = 0x4000;
pub const EV_ONESHOT: u16 = 0x10;
pub const EV_RECEIPT: u16 = 0x40;
pub const EWOULDBLOCK: c_int = 35;
pub const EXDEV: c_int = 18;
pub const EXIT_FAILURE: c_int = 1;
pub const EXIT_SUCCESS: c_int = 0;
pub const EXTPROC: tcflag_t = 0x00000800;
pub const FD_CLOEXEC: c_int = 0x1;
pub const FD_SETSIZE: usize = 1024;
pub const FIOCLEX: c_ulong = 0x20006601;
pub const FIONBIO: c_ulong = 0x8004667e;
pub const FIONCLEX: c_ulong = 0x20006602;
pub const FIONREAD: c_ulong = 0x4004667f;
pub const FLUSHO: tcflag_t = 0x00800000;
pub const F_ALLOCATEALL: c_uint = 0x04;
pub const F_ALLOCATECONTIG: c_uint = 0x02;
pub const F_BARRIERFSYNC: c_int = 85;
pub const F_DUPFD: c_int = 0;
pub const F_DUPFD_CLOEXEC: c_int = 67;
pub const F_FULLFSYNC: c_int = 51;
pub const F_GETFD: c_int = 1;
pub const F_GETFL: c_int = 3;
pub const F_GETLK: c_int = 7;
pub const F_GETPATH: c_int = 50;
pub const F_GETPATH_NOFIRMLINK: c_int = 102;
pub const F_GLOBAL_NOCACHE: c_int = 55;
pub const F_NOCACHE: c_int = 48;
pub const F_OFD_GETLK: c_int = 92;
pub const F_OFD_SETLK: c_int = 90;
pub const F_OFD_SETLKW: c_int = 91;
pub const F_OK: c_int = 0;
pub const F_PEOFPOSMODE: c_int = 3;
pub const F_PREALLOCATE: c_int = 42;
pub const F_RDADVISE: c_int = 44;
pub const F_RDLCK: c_short = 1;
pub const F_SETFD: c_int = 2;
pub const F_SETFL: c_int = 4;
pub const F_SETLK: c_int = 8;
pub const F_SETLKW: c_int = 9;
pub const F_UNLCK: c_short = 2;
pub const F_WRLCK: c_short = 3;
pub const HUPCL: tcflag_t = 0x00004000;
pub const ICANON: tcflag_t = 0x00000100;
pub const ICRNL: tcflag_t = 0x00000100;
pub const IEXTEN: tcflag_t = 0x00000400;
pub const IFF_ALLMULTI: c_int = 0x200;
pub const IFF_ALTPHYS: c_int = 16384;
pub const IFF_BROADCAST: c_int = 0x2;
pub const IFF_DEBUG: c_int = 0x4;
pub const IFF_LINK0: c_int = 0x1000;
pub const IFF_LINK1: c_int = 0x2000;
pub const IFF_LINK2: c_int = 0x4000;
pub const IFF_LOOPBACK: c_int = 0x8;
pub const IFF_MULTICAST: c_int = 0x8000;
pub const IFF_NOARP: c_int = 0x80;
pub const IFF_NOTRAILERS: c_int = 0x20;
pub const IFF_OACTIVE: c_int = 0x400;
pub const IFF_POINTOPOINT: c_int = 0x10;
pub const IFF_PROMISC: c_int = 0x100;
pub const IFF_RUNNING: c_int = 0x40;
pub const IFF_SIMPLEX: c_int = 0x800;
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
pub const IOV_MAX: c_int = 1024;
pub const IPPROTO_ICMP: c_int = 1;
pub const IPPROTO_ICMPV6: c_int = 58;
pub const IPPROTO_IP: c_int = 0;
pub const IPPROTO_IPV6: c_int = 41;
pub const IPPROTO_RAW: c_int = 255;
pub const IPPROTO_TCP: c_int = 6;
pub const IPPROTO_UDP: c_int = 17;
pub const IPV6_DONTFRAG: c_int = 62;
pub const IPV6_JOIN_GROUP: c_int = 12;
pub const IPV6_LEAVE_GROUP: c_int = 13;
pub const IPV6_MULTICAST_HOPS: c_int = 10;
pub const IPV6_RECVPKTINFO: c_int = 61;
pub const IPV6_TCLASS: c_int = 36;
pub const IPV6_UNICAST_HOPS: c_int = 4;
pub const IPV6_V6ONLY: c_int = 27;
pub const IP_ADD_MEMBERSHIP: c_int = 12;
pub const IP_DONTFRAG: c_int = 28;
pub const IP_DROP_MEMBERSHIP: c_int = 13;
pub const IP_MULTICAST_LOOP: c_int = 11;
pub const IP_MULTICAST_TTL: c_int = 10;
pub const IP_PKTINFO: c_int = 26;
pub const IP_RECVDSTADDR: c_int = 7;
pub const IP_RECVIF: c_int = 20;
pub const IP_TOS: c_int = 3;
pub const IP_TTL: c_int = 4;
pub const ISIG: tcflag_t = 0x00000080;
pub const ISTRIP: tcflag_t = 0x00000020;
pub const IUTF8: tcflag_t = 0x00004000;
pub const IXANY: tcflag_t = 0x00000800;
pub const IXOFF: tcflag_t = 0x00000400;
pub const IXON: tcflag_t = 0x00000200;
pub const LC_CTYPE: c_int = 2;
pub const LOCAL_PEERCRED: c_int = 0x001;
pub const LOCAL_PEERPID: c_int = 0x002;
pub const LOCK_EX: c_int = 2;
pub const LOCK_NB: c_int = 4;
pub const LOCK_SH: c_int = 1;
pub const LOCK_UN: c_int = 8;
pub const MADV_CAN_REUSE: c_int = 9;
pub const MADV_DONTNEED: c_int = 4;
pub const MADV_FREE: c_int = 5;
pub const MADV_FREE_REUSABLE: c_int = 7;
pub const MADV_FREE_REUSE: c_int = 8;
pub const MADV_NORMAL: c_int = 0;
pub const MADV_RANDOM: c_int = 1;
pub const MADV_SEQUENTIAL: c_int = 2;
pub const MADV_WILLNEED: c_int = 3;
pub const MADV_ZERO_WIRED_PAGES: c_int = 6;
pub const MAP_ANON: c_int = 0x1000;
pub const MAP_ANONYMOUS: c_int = 4096;
pub const MAP_FAILED: *mut c_void = 0xffffffffffffffff as *mut c_void;
pub const MAP_FILE: c_int = 0x0000;
pub const MAP_FIXED: c_int = 0x0010;
pub const MAP_JIT: c_int = 0x0800;
pub const MAP_NOCACHE: c_int = 0x0400;
pub const MAP_NORESERVE: c_int = 0x0040;
pub const MAP_PRIVATE: c_int = 0x0002;
pub const MAP_SHARED: c_int = 0x0001;
pub const MCL_CURRENT: c_int = 0x0001;
pub const MCL_FUTURE: c_int = 0x0002;
pub const MSG_CTRUNC: c_int = 0x20;
pub const MSG_DONTWAIT: c_int = 0x80;
pub const MSG_EOR: c_int = 0x8;
pub const MSG_NOSIGNAL: c_int = 0x80000;
pub const MSG_OOB: c_int = 0x1;
pub const MSG_PEEK: c_int = 0x2;
pub const MSG_TRUNC: c_int = 0x10;
pub const MSG_WAITALL: c_int = 0x40;
pub const MS_ASYNC: c_int = 0x0001;
pub const MS_DEACTIVATE: c_int = 0x0008;
pub const MS_INVALIDATE: c_int = 0x0002;
pub const MS_KILLPAGES: c_int = 0x0004;
pub const MS_SYNC: c_int = 0x0010;
pub const NCCS: usize = 20;
pub const NETLINK_GENERIC: c_int = 0;
pub const NOFLSH: tcflag_t = 0x80000000;
pub const NOTE_ATTRIB: u32 = 0x00000008;
pub const NOTE_DELETE: u32 = 0x00000001;
pub const NOTE_EXEC: u32 = 0x20000000;
pub const NOTE_EXIT: u32 = 0x80000000;
pub const NOTE_EXITSTATUS: u32 = 0x04000000;
pub const NOTE_EXTEND: u32 = 0x00000004;
pub const NOTE_FFAND: u32 = 0x40000000;
pub const NOTE_FFCOPY: u32 = 0xc0000000;
pub const NOTE_FFCTRLMASK: u32 = 0xc0000000;
pub const NOTE_FFLAGSMASK: u32 = 0x00ffffff;
pub const NOTE_FFNOP: u32 = 0x00000000;
pub const NOTE_FFOR: u32 = 0x80000000;
pub const NOTE_FORK: u32 = 0x40000000;
pub const NOTE_LINK: u32 = 0x00000010;
pub const NOTE_NSECONDS: u32 = 0x00000004;
pub const NOTE_RENAME: u32 = 0x00000020;
pub const NOTE_REVOKE: u32 = 0x00000040;
pub const NOTE_SECONDS: u32 = 0x00000001;
pub const NOTE_TRACK: u32 = 0x00000001;
pub const NOTE_TRACKERR: u32 = 0x00000002;
pub const NOTE_TRIGGER: u32 = 0x01000000;
pub const NOTE_USECONDS: u32 = 0x00000002;
pub const NOTE_WRITE: u32 = 0x00000002;
pub const OCRNL: tcflag_t = 0x00000010;
pub const ONLCR: tcflag_t = 0x2;
pub const ONLRET: tcflag_t = 0x00000040;
pub const ONOCR: tcflag_t = 0x00000020;
pub const OPOST: tcflag_t = 0x1;
pub const O_ACCMODE: c_int = 0x3;
pub const O_APPEND: c_int = 8;
pub const O_ASYNC: c_int = 0x40;
pub const O_CLOEXEC: c_int = 0x01000000;
pub const O_CREAT: c_int = 512;
pub const O_DIRECTORY: c_int = 0x00100000;
pub const O_DSYNC: c_int = 0x00400000;
pub const O_EXCL: c_int = 2048;
pub const O_EXLOCK: c_int = 0x20;
pub const O_FSYNC: c_int = 128;
pub const O_NDELAY: c_int = 4;
pub const O_NOCTTY: c_int = 0x00020000;
pub const O_NOFOLLOW: c_int = 0x100;
pub const O_NONBLOCK: c_int = 0x4;
pub const O_RDONLY: c_int = 0;
pub const O_RDWR: c_int = 2;
pub const O_SEARCH: c_int = 1074790400;
pub const O_SHLOCK: c_int = 0x10;
pub const O_SYNC: c_int = 0x80;
pub const O_TRUNC: c_int = 1024;
pub const O_WRONLY: c_int = 1;
pub const PARENB: tcflag_t = 0x00001000;
pub const PARMRK: tcflag_t = 0x00000008;
pub const PARODD: tcflag_t = 0x00002000;
pub const PATH_MAX: c_int = 1024;
pub const PENDIN: tcflag_t = 0x20000000;
pub const PF_LOCAL: c_int = 1;
pub const PF_ROUTE: c_int = 17;
pub const PIPE_BUF: usize = 512;
pub const POLLERR: c_short = 0x8;
pub const POLLHUP: c_short = 0x10;
pub const POLLIN: c_short = 0x1;
pub const POLLNVAL: c_short = 0x20;
pub const POLLOUT: c_short = 0x4;
pub const POLLPRI: c_short = 0x2;
pub const POLLRDBAND: c_short = 0x080;
pub const POLLRDNORM: c_short = 0x040;
pub const POLLWRBAND: c_short = 0x100;
pub const POLLWRNORM: c_short = 0x004;
pub const PRIO_PGRP: c_int = 1;
pub const PRIO_PROCESS: c_int = 0;
pub const PRIO_USER: c_int = 2;
pub const PROC_PIDTBSDINFO: c_int = 3;
pub const PROT_EXEC: c_int = 4;
pub const PROT_NONE: c_int = 0;
pub const PROT_READ: c_int = 1;
pub const PROT_WRITE: c_int = 2;
pub const P_ALL: idtype_t = 0;
pub const P_PGID: idtype_t = 2;
pub const P_PID: idtype_t = 1;
pub const RENAME_EXCL: c_uint = 0x00000004;
pub const RENAME_SWAP: c_uint = 0x00000002;
pub const RLIMIT_AS: c_int = 5;
pub const RLIMIT_CORE: c_int = 4;
pub const RLIMIT_CPU: c_int = 0;
pub const RLIMIT_DATA: c_int = 2;
pub const RLIMIT_FSIZE: c_int = 1;
pub const RLIMIT_MEMLOCK: c_int = 6;
pub const RLIMIT_NOFILE: c_int = 8;
pub const RLIMIT_NPROC: c_int = 7;
pub const RLIMIT_STACK: c_int = 3;
pub const RLIM_INFINITY: rlim_t = 0x7fff_ffff_ffff_ffff;
pub const RTLD_DEFAULT: *mut c_void = 0xfffffffffffffffe as *mut c_void;
pub const R_OK: c_int = 4;
pub const SA_NOCLDSTOP: c_int = 0x0008;
pub const SA_RESTART: c_int = 0x0002;
pub const SA_SIGINFO: c_int = 0x0040;
pub const SEEK_CUR: c_int = 1;
pub const SEEK_DATA: c_int = 4;
pub const SEEK_END: c_int = 2;
pub const SEEK_HOLE: c_int = 3;
pub const SEEK_SET: c_int = 0;
pub const SF_APPEND: c_uint = 0x00040000;
pub const SF_ARCHIVED: c_uint = 0x00010000;
pub const SF_IMMUTABLE: c_uint = 0x00020000;
pub const SF_SETTABLE: c_uint = 0x3fff0000;
pub const SHUT_RD: c_int = 0;
pub const SHUT_RDWR: c_int = 2;
pub const SHUT_WR: c_int = 1;
pub const SIGABRT: c_int = 6;
pub const SIGALRM: c_int = 14;
pub const SIGBUS: c_int = 10;
pub const SIGCHLD: c_int = 20;
pub const SIGCONT: c_int = 19;
pub const SIGEMT: c_int = 7;
pub const SIGFPE: c_int = 8;
pub const SIGHUP: c_int = 1;
pub const SIGILL: c_int = 4;
pub const SIGINFO: c_int = 29;
pub const SIGINT: c_int = 2;
pub const SIGIO: c_int = 23;
pub const SIGKILL: c_int = 9;
pub const SIGPIPE: c_int = 13;
pub const SIGPROF: c_int = 27;
pub const SIGQUIT: c_int = 3;
pub const SIGSEGV: c_int = 11;
pub const SIGSTOP: c_int = 17;
pub const SIGSYS: c_int = 12;
pub const SIGTERM: c_int = 15;
pub const SIGTRAP: c_int = 5;
pub const SIGTSTP: c_int = 18;
pub const SIGTTIN: c_int = 21;
pub const SIGTTOU: c_int = 22;
pub const SIGURG: c_int = 16;
pub const SIGUSR1: c_int = 30;
pub const SIGUSR2: c_int = 31;
pub const SIGVTALRM: c_int = 26;
pub const SIGWINCH: c_int = 28;
pub const SIGXCPU: c_int = 24;
pub const SIGXFSZ: c_int = 25;
pub const SIG_BLOCK: c_int = 0x1;
pub const SIG_DFL: sighandler_t = 0;
pub const SIG_ERR: sighandler_t = 18446744073709551615;
pub const SIG_IGN: sighandler_t = 1;
pub const SIG_SETMASK: c_int = 3;
pub const SIG_UNBLOCK: c_int = 0x2;
pub const SOCK_DGRAM: c_int = 2;
pub const SOCK_RAW: c_int = 3;
pub const SOCK_RDM: c_int = 4;
pub const SOCK_SEQPACKET: c_int = 5;
pub const SOCK_STREAM: c_int = 1;
pub const SOL_SOCKET: c_int = 0xffff;
pub const SOMAXCONN: c_int = 128;
pub const SO_ACCEPTCONN: c_int = 0x0002;
pub const SO_BROADCAST: c_int = 0x0020;
pub const SO_DONTROUTE: c_int = 0x0010;
pub const SO_ERROR: c_int = 0x1007;
pub const SO_KEEPALIVE: c_int = 0x0008;
pub const SO_LINGER: c_int = 0x0080;
pub const SO_OOBINLINE: c_int = 0x0100;
pub const SO_RCVBUF: c_int = 0x1002;
pub const SO_RCVTIMEO: c_int = 0x1006;
pub const SO_REUSEADDR: c_int = 0x0004;
pub const SO_REUSEPORT: c_int = 0x0200;
pub const SO_SNDBUF: c_int = 0x1001;
pub const SO_SNDTIMEO: c_int = 0x1005;
pub const SO_TIMESTAMP: c_int = 0x0400;
pub const SO_TYPE: c_int = 0x1008;
pub const STDERR_FILENO: c_int = 2;
pub const STDIN_FILENO: c_int = 0;
pub const STDOUT_FILENO: c_int = 1;
pub const ST_NOSUID: c_ulong = 2;
pub const ST_RDONLY: c_ulong = 1;
pub const SYSPROTO_CONTROL: c_int = 2;
pub const SZOMB: u32 = 5;
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
pub const TABDLY: tcflag_t = 0x00000c04;
pub const TCIFLUSH: c_int = 1;
pub const TCIOFF: c_int = 3;
pub const TCIOFLUSH: c_int = 3;
pub const TCION: c_int = 4;
pub const TCOFLUSH: c_int = 2;
pub const TCOOFF: c_int = 1;
pub const TCOON: c_int = 2;
pub const TCP_KEEPALIVE: c_int = 0x10;
pub const TCP_KEEPCNT: c_int = 0x102;
pub const TCP_KEEPINTVL: c_int = 0x101;
pub const TCP_MAXSEG: c_int = 2;
pub const TCP_NODELAY: c_int = 1;
pub const TCSADRAIN: c_int = 1;
pub const TCSAFLUSH: c_int = 2;
pub const TCSANOW: c_int = 0;
pub const TIOCEXCL: c_int = 0x2000740d;
pub const TIOCGWINSZ: c_ulong = 0x40087468;
pub const TIOCNXCL: c_int = 0x2000740e;
pub const TIOCPTYGNAME: c_uint = 0x40807453;
pub const TIOCSCTTY: c_uint = 0x20007461;
pub const TIOCSWINSZ: c_ulong = 0x80087467;
pub const TOSTOP: tcflag_t = 0x00400000;
pub const UF_APPEND: c_uint = 0x00000004;
pub const UF_COMPRESSED: c_uint = 0x00000020;
pub const UF_HIDDEN: c_uint = 0x00008000;
pub const UF_IMMUTABLE: c_uint = 0x00000002;
pub const UF_NODUMP: c_uint = 0x00000001;
pub const UF_OPAQUE: c_uint = 0x00000008;
pub const UF_SETTABLE: c_uint = 0x0000ffff;
pub const UF_TRACKED: c_uint = 0x00000040;
pub const UTIME_NOW: c_long = -1;
pub const UTIME_OMIT: c_long = -2;
pub const UTUN_OPT_IFNAME: c_int = 2;
pub const VDISCARD: usize = 15;
pub const VDSUSP: usize = 11;
pub const VEOF: usize = 0;
pub const VEOL: usize = 1;
pub const VEOL2: usize = 2;
pub const VERASE: usize = 3;
pub const VINTR: usize = 8;
pub const VKILL: usize = 5;
pub const VLNEXT: usize = 14;
pub const VMIN: usize = 16;
pub const VQUIT: usize = 9;
pub const VREPRINT: usize = 6;
pub const VSTART: usize = 12;
pub const VSTATUS: usize = 18;
pub const VSTOP: usize = 13;
pub const VSUSP: usize = 10;
pub const VTIME: usize = 17;
pub const VWERASE: usize = 4;
pub const WCONTINUED: c_int = 0x00000010;
pub const WEXITED: c_int = 0x00000004;
pub const WNOHANG: c_int = 0x00000001;
pub const WNOWAIT: c_int = 0x00000020;
pub const WSTOPPED: c_int = 0x00000008;
pub const WUNTRACED: c_int = 0x00000002;
pub const W_OK: c_int = 2;
pub const XATTR_CREATE: c_int = 0x0002;
pub const XATTR_NOFOLLOW: c_int = 0x0001;
pub const XATTR_REPLACE: c_int = 0x0004;
pub const X_OK: c_int = 1;
pub const _SC_CLK_TCK: c_int = 3;
pub const _SC_PAGESIZE: c_int = 29;

/// PTHREAD_COND_INITIALIZER is a struct-valued initializer whose contents are opaque in
/// libc. Its 48 bytes were read out of the platform ABI by a
/// const-eval probe compiled for aarch64-apple-darwin and are asserted byte-exact
/// against the oracle.
pub const PTHREAD_COND_INITIALIZER: pthread_cond_t = unsafe {
    ::core::mem::transmute::<[u8; 48], pthread_cond_t>([
        0xbb, 0xb1, 0xb0, 0x3c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00,
    ])
};

/// PTHREAD_MUTEX_INITIALIZER is a struct-valued initializer whose contents are opaque in
/// libc. Its 64 bytes were read out of the platform ABI by a
/// const-eval probe compiled for aarch64-apple-darwin and are asserted byte-exact
/// against the oracle.
pub const PTHREAD_MUTEX_INITIALIZER: pthread_mutex_t = unsafe {
    ::core::mem::transmute::<[u8; 64], pthread_mutex_t>([
        0xa7, 0xab, 0xaa, 0x32, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00,
    ])
};

// ------------------------------------------------------------ functions
unsafe extern "C" {
    pub fn __error() -> *mut c_int;
    pub fn _exit(status: c_int) -> !;
    pub fn abort() -> !;
    pub fn accept(socket: c_int, address: *mut sockaddr, address_len: *mut socklen_t) -> c_int;
    pub fn access(path: *const c_char, amode: c_int) -> c_int;
    pub fn bind(socket: c_int, address: *const sockaddr, address_len: socklen_t) -> c_int;
    pub fn cfgetispeed(termios: *const termios) -> speed_t;
    pub fn cfgetospeed(termios: *const termios) -> speed_t;
    pub fn cfmakeraw(termios: *mut termios);
    pub fn cfsetispeed(termios: *mut termios, speed: speed_t) -> c_int;
    pub fn cfsetospeed(termios: *mut termios, speed: speed_t) -> c_int;
    pub fn cfsetspeed(termios: *mut termios, speed: speed_t) -> c_int;
    pub fn chdir(dir: *const c_char) -> c_int;
    pub fn chflags(path: *const c_char, flags: c_uint) -> c_int;
    pub fn chmod(path: *const c_char, mode: mode_t) -> c_int;
    pub fn chown(path: *const c_char, uid: uid_t, gid: gid_t) -> c_int;
    pub fn chroot(name: *const c_char) -> c_int;
    pub fn clock_getres(clk_id: clockid_t, tp: *mut timespec) -> c_int;
    pub fn clock_gettime(clk_id: clockid_t, tp: *mut timespec) -> c_int;
    pub fn clock_settime(clock_id: clockid_t, tp: *const timespec) -> c_int;
    pub fn close(fd: c_int) -> c_int;
    pub fn closedir(dirp: *mut DIR) -> c_int;
    pub fn connect(socket: c_int, address: *const sockaddr, len: socklen_t) -> c_int;
    pub fn dirfd(dirp: *mut DIR) -> c_int;
    pub fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    pub fn dup(fd: c_int) -> c_int;
    pub fn dup2(src: c_int, dst: c_int) -> c_int;
    pub fn execv(prog: *const c_char, argv: *const *const c_char) -> c_int;
    pub fn execve(
        prog: *const c_char,
        argv: *const *const c_char,
        envp: *const *const c_char,
    ) -> c_int;
    pub fn execvp(c: *const c_char, argv: *const *const c_char) -> c_int;
    pub fn faccessat(dirfd: c_int, pathname: *const c_char, mode: c_int, flags: c_int) -> c_int;
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
    pub fn fdopendir(fd: c_int) -> *mut DIR;
    pub fn fgetxattr(
        filedes: c_int,
        name: *const c_char,
        value: *mut c_void,
        size: size_t,
        position: u32,
        flags: c_int,
    ) -> ssize_t;
    pub fn fileno(stream: *mut FILE) -> c_int;
    pub fn flistxattr(filedes: c_int, list: *mut c_char, size: size_t, flags: c_int) -> ssize_t;
    pub fn flock(fd: c_int, operation: c_int) -> c_int;
    pub fn fork() -> pid_t;
    pub fn forkpty(
        amaster: *mut c_int,
        name: *mut c_char,
        termp: *mut termios,
        winp: *mut winsize,
    ) -> pid_t;
    pub fn freeifaddrs(ifa: *mut ifaddrs);
    pub fn fremovexattr(filedes: c_int, name: *const c_char, flags: c_int) -> c_int;
    pub fn fsetxattr(
        filedes: c_int,
        name: *const c_char,
        value: *const c_void,
        size: size_t,
        position: u32,
        flags: c_int,
    ) -> c_int;
    pub fn fstat(fildes: c_int, buf: *mut stat) -> c_int;
    pub fn fstatat(dirfd: c_int, pathname: *const c_char, buf: *mut stat, flags: c_int) -> c_int;
    pub fn fstatfs(fd: c_int, buf: *mut statfs) -> c_int;
    pub fn fstatvfs(fd: c_int, buf: *mut statvfs) -> c_int;
    pub fn fsync(fd: c_int) -> c_int;
    pub fn ftruncate(fd: c_int, length: off_t) -> c_int;
    pub fn futimens(fd: c_int, times: *const timespec) -> c_int;
    pub fn getcwd(buf: *mut c_char, size: size_t) -> *mut c_char;
    pub fn getegid() -> gid_t;
    pub fn getentropy(buf: *mut c_void, buflen: size_t) -> c_int;
    pub fn geteuid() -> uid_t;
    pub fn getgid() -> gid_t;
    pub fn getgroups(ngroups_max: c_int, groups: *mut gid_t) -> c_int;
    pub fn getifaddrs(ifap: *mut *mut ifaddrs) -> c_int;
    pub fn getpeereid(socket: c_int, euid: *mut uid_t, egid: *mut gid_t) -> c_int;
    pub fn getpeername(socket: c_int, address: *mut sockaddr, address_len: *mut socklen_t)
    -> c_int;
    pub fn getpgid(pid: pid_t) -> pid_t;
    pub fn getpgrp() -> pid_t;
    pub fn getpid() -> pid_t;
    pub fn getppid() -> pid_t;
    pub fn getpriority(which: c_int, who: id_t) -> c_int;
    pub fn getpwuid_r(
        uid: uid_t,
        pwd: *mut passwd,
        buf: *mut c_char,
        buflen: size_t,
        result: *mut *mut passwd,
    ) -> c_int;
    pub fn getrlimit(resource: c_int, rlim: *mut rlimit) -> c_int;
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
    pub fn gettimeofday(tp: *mut timeval, tz: *mut c_void) -> c_int;
    pub fn getuid() -> uid_t;
    pub fn getxattr(
        path: *const c_char,
        name: *const c_char,
        value: *mut c_void,
        size: size_t,
        position: u32,
        flags: c_int,
    ) -> ssize_t;
    pub fn grantpt(fd: c_int) -> c_int;
    pub fn host_statistics64(
        host_priv: host_t,
        flavor: host_flavor_t,
        host_info64_out: host_info64_t,
        host_info64_outCnt: *mut mach_msg_type_number_t,
    ) -> kern_return_t;
    pub fn if_freenameindex(ptr: *mut if_nameindex);
    pub fn if_indextoname(ifindex: c_uint, ifname: *mut c_char) -> *mut c_char;
    pub fn if_nameindex() -> *mut if_nameindex;
    pub fn if_nametoindex(ifname: *const c_char) -> c_uint;
    pub fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    pub fn isatty(fd: c_int) -> c_int;
    pub fn kevent(
        kq: c_int,
        changelist: *const kevent,
        nchanges: c_int,
        eventlist: *mut kevent,
        nevents: c_int,
        timeout: *const timespec,
    ) -> c_int;
    pub fn kill(pid: pid_t, sig: c_int) -> c_int;
    pub fn killpg(pgrp: pid_t, sig: c_int) -> c_int;
    pub fn kqueue() -> c_int;
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
    pub fn listxattr(path: *const c_char, list: *mut c_char, size: size_t, flags: c_int)
    -> ssize_t;
    pub fn login_tty(fd: c_int) -> c_int;
    pub fn lseek(fd: c_int, offset: off_t, whence: c_int) -> off_t;
    pub fn lstat(path: *const c_char, buf: *mut stat) -> c_int;
    pub fn lutimes(file: *const c_char, times: *const timeval) -> c_int;
    pub fn madvise(addr: *mut c_void, len: size_t, advice: c_int) -> c_int;
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
    pub fn mprotect(addr: *mut c_void, len: size_t, prot: c_int) -> c_int;
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
        termp: *mut termios,
        winp: *mut winsize,
    ) -> c_int;
    pub fn pause() -> c_int;
    pub fn pipe(fds: *mut c_int) -> c_int;
    pub fn poll(fds: *mut pollfd, nfds: nfds_t, timeout: c_int) -> c_int;
    pub fn posix_openpt(flags: c_int) -> c_int;
    pub fn pread(fd: c_int, buf: *mut c_void, count: size_t, offset: off_t) -> ssize_t;
    pub fn proc_pidinfo(
        pid: c_int,
        flavor: c_int,
        arg: u64,
        buffer: *mut c_void,
        buffersize: c_int,
    ) -> c_int;
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
    pub fn pthread_kill(thread: pthread_t, sig: c_int) -> c_int;
    pub fn pthread_main_np() -> c_int;
    pub fn pthread_mutex_destroy(lock: *mut pthread_mutex_t) -> c_int;
    pub fn pthread_mutex_lock(lock: *mut pthread_mutex_t) -> c_int;
    pub fn pthread_mutex_unlock(lock: *mut pthread_mutex_t) -> c_int;
    pub fn pthread_set_qos_class_self_np(class: qos_class_t, priority: c_int) -> c_int;
    pub fn pthread_sigmask(how: c_int, set: *const sigset_t, oldset: *mut sigset_t) -> c_int;
    pub fn ptsname(fd: c_int) -> *mut c_char;
    pub fn pwrite(fd: c_int, buf: *const c_void, count: size_t, offset: off_t) -> ssize_t;
    pub fn raise(signum: c_int) -> c_int;
    pub fn read(fd: c_int, buf: *mut c_void, count: size_t) -> ssize_t;
    pub fn readdir(dirp: *mut DIR) -> *mut dirent;
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
    pub fn removexattr(path: *const c_char, name: *const c_char, flags: c_int) -> c_int;
    pub fn rename(oldname: *const c_char, newname: *const c_char) -> c_int;
    pub fn renameat(
        olddirfd: c_int,
        oldpath: *const c_char,
        newdirfd: c_int,
        newpath: *const c_char,
    ) -> c_int;
    pub fn renameatx_np(
        fromfd: c_int,
        from: *const c_char,
        tofd: c_int,
        to: *const c_char,
        flags: c_uint,
    ) -> c_int;
    pub fn renamex_np(from: *const c_char, to: *const c_char, flags: c_uint) -> c_int;
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
    pub fn setpriority(which: c_int, who: id_t, prio: c_int) -> c_int;
    pub fn setrlimit(resource: c_int, rlim: *const rlimit) -> c_int;
    pub fn setsid() -> pid_t;
    pub fn setsockopt(
        socket: c_int,
        level: c_int,
        name: c_int,
        value: *const c_void,
        option_len: socklen_t,
    ) -> c_int;
    pub fn setxattr(
        path: *const c_char,
        name: *const c_char,
        value: *const c_void,
        size: size_t,
        position: u32,
        flags: c_int,
    ) -> c_int;
    pub fn shm_open(name: *const c_char, oflag: c_int, ...) -> c_int;
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
    pub fn statvfs(path: *const c_char, buf: *mut statvfs) -> c_int;
    pub fn strerror_r(errnum: c_int, buf: *mut c_char, buflen: size_t) -> c_int;
    pub fn strlen(cs: *const c_char) -> size_t;
    pub fn symlink(path1: *const c_char, path2: *const c_char) -> c_int;
    pub fn symlinkat(target: *const c_char, newdirfd: c_int, linkpath: *const c_char) -> c_int;
    pub fn sync();
    pub fn syscall(num: c_int, ...) -> c_int;
    pub fn sysconf(name: c_int) -> c_long;
    pub fn sysctl(
        name: *mut c_int,
        namelen: c_uint,
        oldp: *mut c_void,
        oldlenp: *mut size_t,
        newp: *mut c_void,
        newlen: size_t,
    ) -> c_int;
    pub fn sysctlbyname(
        name: *const c_char,
        oldp: *mut c_void,
        oldlenp: *mut size_t,
        newp: *mut c_void,
        newlen: size_t,
    ) -> c_int;
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
    pub fn ttyname_r(fd: c_int, buf: *mut c_char, buflen: size_t) -> c_int;
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
type fd_word = u32;

/// # Safety
///
/// `set` must point to a valid, initialised `fd_set`, and `fd` must be in
/// `0..FD_SETSIZE` — out of range reads past the end of the bitmap.
#[inline]
pub unsafe extern "C" fn FD_ISSET(fd: c_int, set: *const fd_set) -> bool {
    let bits = ::core::mem::size_of::<fd_word>() * 8;
    let fd = fd as usize;
    unsafe { ((*set).__opaque[fd / bits] & (1 << (fd % bits))) != 0 }
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
    (status >> 8) & 0x00ff
}

#[inline]
pub const extern "C" fn WIFCONTINUED(status: c_int) -> bool {
    (status & 0o177) == 0o177 && (status >> 8) == 0x13
}

#[inline]
pub const extern "C" fn WIFEXITED(status: c_int) -> bool {
    (status & 0o177) == 0
}

#[inline]
pub const extern "C" fn WIFSIGNALED(status: c_int) -> bool {
    (status & 0o177) != 0o177 && (status & 0o177) != 0
}

#[inline]
pub const extern "C" fn WIFSTOPPED(status: c_int) -> bool {
    (status & 0o177) == 0o177 && (status >> 8) != 0x13
}

#[inline]
pub const extern "C" fn WSTOPSIG(status: c_int) -> c_int {
    status >> 8
}

#[inline]
pub const extern "C" fn WTERMSIG(status: c_int) -> c_int {
    status & 0o177
}

#[inline]
pub const extern "C" fn major(dev: dev_t) -> i32 {
    (dev >> 24) & 0xff
}

#[inline]
pub const extern "C" fn makedev(major: i32, minor: i32) -> dev_t {
    (major << 24) | minor
}

#[inline]
pub const extern "C" fn minor(dev: dev_t) -> i32 {
    dev & 0xff_ffff
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
        self.si_addr
    }

    /// # Safety
    ///
    /// The caller must know the signal was a SIGCHLD; this reads a union arm.
    pub unsafe fn si_pid(&self) -> pid_t {
        self.si_pid
    }

    /// # Safety
    ///
    /// The caller must know the signal was a SIGCHLD; this reads a union arm.
    pub unsafe fn si_uid(&self) -> uid_t {
        self.si_uid
    }

    /// # Safety
    ///
    /// The caller must know the signal was a SIGCHLD; this reads a union arm.
    pub unsafe fn si_status(&self) -> c_int {
        self.si_status
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
            _si_pid: pid_t,
            _si_uid: uid_t,
            _si_status: c_int,
            _si_addr: *mut c_void,
            si_value: sigval,
        }
        unsafe { (*(self as *const siginfo_t).cast::<siginfo_timer>()).si_value }
    }
}
