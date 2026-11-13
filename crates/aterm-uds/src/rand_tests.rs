// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::hex_encode;

/// Each output pair depends on its input byte alone, so every byte value is the whole
/// domain: each encodes to two lowercase-hex chars that `u8::from_str_radix` maps back,
/// and a longer input is the concatenation of its bytes' pairs.
#[test]
fn hex_encode_is_exact_lowercase_and_injective() {
    let pair = |b: u8| hex_encode(&[b]);
    let mut seen = std::collections::BTreeSet::new();
    for b in 0..=u8::MAX {
        let hex = pair(b);
        assert_eq!(hex.len(), 2, "{b:#04x}");
        assert!(
            hex.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            "{b:#04x} -> {hex:?} is not lowercase hex"
        );
        assert_eq!(u8::from_str_radix(&hex, 16), Ok(b), "{hex:?} decodes back");
        assert!(seen.insert(hex), "{b:#04x} prints like another byte");
    }
    assert_eq!(hex_encode(&[]), "");
    fn concat<const N: usize>() {
        for pos in 0..N {
            for b in 0..=u8::MAX {
                let mut bytes = [0x5a; N];
                bytes[pos] = b;
                let want: String = bytes.iter().map(|&x| hex_encode(&[x])).collect();
                assert_eq!(want.len(), 2 * N);
                assert_eq!(hex_encode(&bytes), want, "N={N} pos={pos} b={b:#04x}");
            }
        }
    }
    concat::<8>();
    concat::<16>();
}

/// The source is asked in chunks of at most 256 bytes (getentropy(2)'s cap), and a source
/// that fills everything leaves the (nonexistent) device unopened.
#[cfg(unix)]
#[test]
fn the_source_is_asked_in_chunks_of_at_most_256_and_the_device_is_left_alone() {
    let mut asked = Vec::new();
    let mut buf = [0u8; 600];
    let r = super::fill_from(
        &mut buf,
        |chunk| {
            asked.push(chunk.len());
            chunk.fill(0x5a);
            true
        },
        "/nonexistent/aterm-uds-rand-device",
    );
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(asked, [256, 256, 88]);
    assert!(buf.iter().all(|&b| b == 0x5a));
    let mut empty = [0u8; 0];
    let r = super::fill_from(&mut empty, |_| false, "/nonexistent/aterm-uds-rand-device");
    assert!(r.is_ok(), "an empty buffer asks nothing: {r:?}");
}

/// A forced failure is an error, never a partly filled buffer: a failing source with a
/// missing device, or one chunk filled and a device that ends early (`/dev/null`). A
/// fallback that succeeds (`/dev/zero`) replaces the whole buffer.
#[cfg(unix)]
#[test]
fn a_forced_failure_is_an_error_and_the_fallback_rewrites_the_whole_buffer() {
    let mut buf = [0xa5u8; 600];
    let r = super::fill_from(&mut buf, |_| false, "/nonexistent/aterm-uds-rand-device");
    assert_eq!(r.map_err(|e| e.kind()), Err(std::io::ErrorKind::NotFound));

    let mut first = true;
    let mut buf = [0xa5u8; 600];
    let r = super::fill_from(
        &mut buf,
        |chunk| {
            chunk.fill(0x11);
            std::mem::replace(&mut first, false)
        },
        "/dev/null",
    );
    assert_eq!(
        r.map_err(|e| e.kind()),
        Err(std::io::ErrorKind::UnexpectedEof)
    );

    let mut first = true;
    let mut buf = [0xa5u8; 600];
    let r = super::fill_from(
        &mut buf,
        |chunk| {
            chunk.fill(0x11);
            std::mem::replace(&mut first, false)
        },
        "/dev/zero",
    );
    assert!(r.is_ok(), "{r:?}");
    assert!(
        buf.iter().all(|&b| b == 0),
        "the device's bytes, all 600 of them"
    );
}

/// The readiness poll returns on a readable device and reports a missing one; it compiles
/// on every Unix under test, so this runs on macOS too.
#[cfg(unix)]
#[test]
fn the_readiness_poll_returns_on_dev_random_and_reports_a_missing_device() {
    assert!(super::wait_readable("/dev/random").is_ok());
    assert!(super::wait_until_rng_ready().is_ok());
    assert!(
        super::wait_until_rng_ready().is_ok(),
        "the remembered answer"
    );
    let r = super::wait_readable("/nonexistent/aterm-uds-rand-device");
    assert_eq!(r.map_err(|e| e.kind()), Err(std::io::ErrorKind::NotFound));
}
