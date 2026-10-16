// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the `sudo` askpass primitives. Behaviour and security only — no
//! wording pins, no timing. These run as an ordinary test binary (no sudo
//! parent), which is itself a useful control: the test process is NOT a child
//! of `/usr/bin/sudo`, so `attest_sudo` on its own parent must refuse.

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

    // The Debug never prints the bytes.
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
        // The parent exists (the test runner), so this is a real refusal by
        // the facts, not a "gone" miss.
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

/// A pid that cannot be real is refused, not treated as readable.
#[test]
fn attesting_a_bogus_pid_refuses() {
    assert!(attest_sudo(-1).is_err());
    assert!(attest_sudo(0).is_err());
    assert!(parent_of(-1).is_none());
    assert!(exe_path(0).is_none());
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
    assert!(WireBuffer::from_bytes(&[b'x'; 2066]).is_none());
    for bad in [b"OK incomplete".as_slice(), b"OK\nextra"] {
        assert!(WireBuffer::read_line(&mut Cursor::new(bad)).is_err());
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
}
