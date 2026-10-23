// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the `sudo` askpass primitives.

use super::*;

/// A `Secret` gives back exactly the bytes it was built from, caps at 1023,
/// reports empty honestly, and redacts its `Debug`. (The scrub-on-drop cannot
/// be observed safely after the box is freed; the positive control here is
/// that the bytes are correct while alive — the drop path is exercised, not
/// asserted.)
#[test]
fn secret_round_trips_caps_and_redacts() {
    let s = Secret::from_bytes(b"hunter2");
    assert_eq!(s.as_bytes(), b"hunter2");
    assert!(!s.is_empty());

    let empty = Secret::from_bytes(b"");
    assert!(empty.is_empty());
    assert_eq!(empty.as_bytes(), b"");

    let long = vec![b'x'; 5000];
    let capped = Secret::from_bytes(&long);
    assert_eq!(capped.as_bytes().len(), 1023);

    let shown = format!("{:?}", Secret::from_bytes(b"topsecret"));
    assert!(shown.contains("redacted"), "{shown}");
    assert!(!shown.contains("topsecret"), "{shown}");
}

/// The test binary's own parent is not `/usr/bin/sudo`, so attesting it refuses
/// with a reason, never `Ok`. This is the direct-call attack: a same-uid
/// process that is not under sudo must not pass. (On non-Apple platforms the
/// call refuses by construction.)
#[test]
fn attesting_a_non_sudo_parent_refuses() {
    let ppid = parent_pid();
    #[cfg(target_vendor = "apple")]
    {
        assert!(ppid > 0, "a test has a parent");
        let r = attest_sudo(ppid);
        assert!(r.is_err(), "the test runner is not /usr/bin/sudo: {r:?}");
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = ppid;
        assert!(attest_sudo(1).is_err());
    }
}

/// A pid that cannot be real is refused, not treated as readable: pid 0 is
/// the kernel's (Darwin answers its short info), and no process has pid
/// `i32::MAX`.
#[test]
fn attesting_a_bogus_pid_refuses() {
    assert!(attest_sudo(-1).is_err());
    assert!(attest_sudo(0).is_err());
    assert!(parent_of(-1).is_none());
    assert!(parent_of(0).is_none());
    assert!(exe_path(0).is_none());
    assert!(exe_path(i32::MAX).is_none());
}

/// The facts read about a live process are its own: a child's parent (this
/// process, where a test runner's child shares its parent's group, so the
/// parent read must not be the group), this process's effective and real
/// uid; launchd is a valid platform binary and this test binary is not.
#[cfg(target_vendor = "apple")]
#[test]
fn the_facts_read_are_the_processs_own() {
    unsafe extern "C" {
        fn geteuid() -> u32;
        fn getuid() -> u32;
    }
    let me = unsafe { imp_getpid() };
    let mut child = crate::spawnfd::command("/bin/cat")
        .stdin(crate::spawnfd::Stdio::piped())
        .spawn()
        .expect("spawn");
    let parent = parent_of(i32::try_from(child.id()).expect("pid"));
    drop(child.stdin.take());
    child.wait().expect("reaped");
    assert_eq!(parent, Some(me));
    let info = imp::short_info(me).expect("our own short info");
    // SAFETY: plain getters.
    assert_eq!((info.uid, info.ruid), unsafe { (geteuid(), getuid()) });
    assert!(imp::cs_platform_valid(1), "launchd");
    assert!(
        !imp::cs_platform_valid(me),
        "a test binary is no platform binary"
    );
}

/// Every attested fact refuses alone; all of them together admit.
#[cfg(target_vendor = "apple")]
#[test]
fn each_attested_fact_refuses_alone() {
    use imp::{ShortInfo, attest};
    use std::path::PathBuf;
    let sudo = ShortInfo {
        ppid: 1,
        uid: 0,
        ruid: 501,
    };
    let at = |path: &str| Some(PathBuf::from(path));
    assert_eq!(
        attest(Some(sudo), 501, || at("/usr/bin/sudo"), || true),
        Ok(())
    );
    for (why, info, exe, platform) in [
        ("gone", None, at("/usr/bin/sudo"), true),
        (
            "not root",
            Some(ShortInfo { uid: 501, ..sudo }),
            at("/usr/bin/sudo"),
            true,
        ),
        (
            "another real uid",
            Some(ShortInfo { ruid: 502, ..sudo }),
            at("/usr/bin/sudo"),
            true,
        ),
        ("another program", Some(sudo), at("/tmp/sudo"), true),
        ("no executable", Some(sudo), None, true),
        (
            "not a platform binary",
            Some(sudo),
            at("/usr/bin/sudo"),
            false,
        ),
    ] {
        assert!(attest(info, 501, || exe, || platform).is_err(), "{why}");
    }
}

/// Only fd 1's own record decides, and only an anonymous pipe there admits.
#[cfg(target_vendor = "apple")]
#[test]
fn only_an_anonymous_pipe_at_fd_one_is_stdout() {
    const VNODE: u32 = 1;
    const PIPE: u32 = 6;
    let table = |records: &[(i32, u32)]| -> Vec<u8> {
        records
            .iter()
            .flat_map(|&(fd, ty)| fd.to_ne_bytes().into_iter().chain(ty.to_ne_bytes()))
            .collect()
    };
    assert_eq!(
        imp::stdout_record_is_anon_pipe(&table(&[(0, VNODE), (1, PIPE)])),
        Ok(())
    );
    for (why, records) in [
        ("a file at fd 1", &[(0, PIPE), (1, VNODE)][..]),
        ("no fd 1", &[(0, PIPE), (2, PIPE)][..]),
        ("an empty table", &[][..]),
    ] {
        assert!(
            imp::stdout_record_is_anon_pipe(&table(records)).is_err(),
            "{why}"
        );
    }
}

/// The live fd table decides: a child whose fd 1 is the anonymous pipe it was
/// handed is admitted, and one whose fd 1 is a file or a socket is refused.
#[cfg(target_vendor = "apple")]
#[test]
fn fd_one_is_read_from_the_live_table() {
    use crate::spawnfd::Stdio;
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-uds-sudo-fd1.")
        .tempdir()
        .expect("scratch");
    let file = std::fs::File::create(scratch.path().join("out")).expect("file");
    let (socket, _peer) = crate::CtlStream::pair().expect("pair");
    for (why, stdout, admitted) in [
        ("a pipe", Stdio::piped(), true),
        ("a file", Stdio::from(file), false),
        (
            "a socket",
            Stdio::from(std::os::fd::OwnedFd::from(socket)),
            false,
        ),
    ] {
        let mut child = crate::spawnfd::command("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(stdout)
            .spawn()
            .expect("spawn");
        let verdict = imp::fd_one_is_anon_pipe(i32::try_from(child.id()).expect("pid"));
        drop(child.stdin.take());
        child.wait().expect("reaped");
        assert_eq!(verdict.is_ok(), admitted, "{why}: {verdict:?}");
    }
}

/// This process can read its OWN executable path (a positive control that
/// `exe_path` works), and it is not `/usr/bin/sudo`.
#[cfg(target_vendor = "apple")]
#[test]
fn exe_path_reads_our_own_and_it_is_not_sudo() {
    let me = unsafe { imp_getpid() };
    let p = exe_path(me).expect("our own exe path is readable");
    assert!(p.is_absolute(), "{p:?}");
    assert_ne!(p, std::path::Path::new("/usr/bin/sudo"));
}

#[cfg(target_vendor = "apple")]
unsafe extern "C" {
    #[link_name = "getpid"]
    fn imp_getpid() -> i32;
}

#[test]
fn guarded_codec_round_trips_the_limit_and_refuses_partial_invalid_inputs() {
    for source in [b"public pw".as_slice(), &[0x80, 0xff], &vec![b'x'; 1023]] {
        let secret = Secret::from_bytes(source);
        let wire = WireBuffer::secret_reply(&secret);
        let hex = wire
            .as_bytes()
            .strip_prefix(b"OK askpass secret=")
            .unwrap()
            .strip_suffix(b"\n")
            .unwrap();
        assert_eq!(Secret::decode_hex(hex).unwrap().as_bytes(), source);
    }
    assert_eq!(Secret::decode_hex(b"4A4aFf").unwrap().as_bytes(), b"JJ\xff");
    for bad in [b"7077zz".as_slice(), b"707700", b"70770d", b"70770a", b"7"] {
        assert!(Secret::decode_hex(bad).is_none());
    }
    assert!(Secret::decode_hex(&vec![b'6'; 2048]).is_none());
    let shown = format!(
        "{:?}",
        WireBuffer::secret_reply(&Secret::from_bytes(b"public"))
    );
    assert!(!shown.contains("7075626c6963"));
}

#[test]
fn secure_utf16_conversion_is_exact_and_never_truncates() {
    let units = "public é 🦀".encode_utf16().collect::<Vec<_>>();
    let mut field = SecretUtf16::new(units.len()).unwrap();
    field.as_mut_units().copy_from_slice(&units);
    assert_eq!(
        Secret::from_utf16(field.as_units()).unwrap().as_bytes(),
        "public é 🦀".as_bytes()
    );
    assert!(SecretUtf16::new(1023).is_some());
    assert!(SecretUtf16::new(1024).is_none());
    assert!(Secret::from_utf16(&[b'p' as u16, 0xd800]).is_none());
    for cut in [0, 10, 13] {
        assert!(Secret::from_utf16(&[b'p' as u16, cut]).is_none());
    }
    assert_eq!(
        Secret::from_utf16(&[b'x' as u16; 1023])
            .unwrap()
            .as_bytes()
            .len(),
        1023
    );
    assert!(Secret::from_utf16(&[0x00e9; 512]).is_none());
}

#[test]
fn guarded_wire_reader_requires_one_complete_bounded_frame() {
    use std::io::{Cursor, ErrorKind};
    let max = WireBuffer::secret_reply(&Secret::from_bytes(&[b'x'; 1023]));
    assert_eq!(
        WireBuffer::read_line(&mut Cursor::new(max.as_bytes()))
            .unwrap()
            .as_bytes(),
        max.as_bytes()
    );
    assert!(WireBuffer::from_bytes(&[b'x'; 2065]).is_some());
    assert!(WireBuffer::from_bytes(&[b'x'; 2066]).is_none());
    assert_eq!(
        WireBuffer::password_line(&Secret::from_bytes(b"pw")).as_bytes(),
        b"pw\n"
    );
    for (bad, kind) in [
        (b"OK incomplete".as_slice(), ErrorKind::UnexpectedEof),
        (b"OK\nextra", ErrorKind::InvalidData),
    ] {
        let err = WireBuffer::read_line(&mut Cursor::new(bad)).unwrap_err();
        assert_eq!(err.kind(), kind);
    }
    let too_long = [b'x'; 2066];
    assert_eq!(
        WireBuffer::read_line(&mut Cursor::new(too_long))
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidData
    );
    struct PartialFailure(bool);
    impl std::io::Read for PartialFailure {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.0 {
                return Err(ErrorKind::ConnectionReset.into());
            }
            self.0 = true;
            out[..2].copy_from_slice(b"70");
            Ok(2)
        }
    }
    assert_eq!(
        WireBuffer::read_line(&mut PartialFailure(false))
            .unwrap_err()
            .kind(),
        ErrorKind::ConnectionReset
    );
    /// A signal before the line: the read is asked again.
    struct InterruptedOnce(bool);
    impl std::io::Read for InterruptedOnce {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if !std::mem::replace(&mut self.0, true) {
                return Err(ErrorKind::Interrupted.into());
            }
            out[..3].copy_from_slice(b"OK\n");
            Ok(3)
        }
    }
    assert_eq!(
        WireBuffer::read_line(&mut InterruptedOnce(false))
            .unwrap()
            .as_bytes(),
        b"OK\n"
    );
}
