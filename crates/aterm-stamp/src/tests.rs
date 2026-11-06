// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn stamped() -> Stamp {
    Stamp {
        app_version: "0.157.0",
        git_commit: "0123456789ab-dirty",
        git_commit_full: "0123456789abcdef0123456789abcdef01234567",
        git_dirty: "true",
        build_time: "2026-11-05T00:00:00Z",
        build_number: "1793930597",
        dev_commits: "12",
        index_hint: "16000..16387",
        release_build: true,
        compiler_flavor: "t",
        ..Stamp::UNSTAMPED
    }
}

fn leak(record: [u8; LEN]) -> &'static [u8; LEN] {
    Box::leak(Box::new(record))
}

fn record(text: &str) -> &'static [u8; LEN] {
    let mut out = [0u8; LEN];
    out[..text.len()].copy_from_slice(text.as_bytes());
    leak(out)
}

#[test]
fn a_record_reads_back_every_identity_field_over_the_compile_facts() {
    let compiled = Stamp {
        compiler_flavor: "t",
        build_profile: "release",
        ..Stamp::UNSTAMPED
    };
    let bytes = encode(&stamped()).unwrap();
    assert!(bytes.starts_with(HEADER.as_bytes()));
    let read = decode(leak(bytes), compiled).unwrap();
    assert_eq!(
        read,
        Stamp {
            build_profile: "release",
            ..stamped()
        }
    );
    // The compile facts are the binary's, never the record's.
    let other = decode(leak(bytes), Stamp::UNSTAMPED).unwrap();
    assert_eq!(other.compiler_flavor, "r");
    assert_eq!(other.build_id(), "0.157.0+g0123456789ab-dirty");
}

#[test]
fn a_malformed_record_is_refused() {
    let good = String::from_utf8(encode(&stamped()).unwrap().to_vec()).unwrap();
    let good = good.trim_end_matches('\0');
    let cases = [
        good.replacen("aterm-stamp 1", "aterm-stamp 2", 1),
        good.replacen("dirty=true", "dirty=yes", 1),
        good.replacen("release=true", "release=1", 1),
        good.replacen("commit=", "comit=", 1),
        good.replacen("built_at=2026-11-05T00:00:00Z\n", "", 1),
        format!("{good}extra=1\n"),
        String::new(),
    ];
    for text in &cases {
        assert!(decode(record(text), Stamp::UNSTAMPED).is_err(), "{text:?}");
    }
    assert!(decode(record(good), Stamp::UNSTAMPED).is_ok());
}

#[test]
fn a_value_that_would_break_the_record_is_not_written() {
    for stamp in [
        Stamp {
            git_commit: "a\nversion=9.9.9",
            ..stamped()
        },
        Stamp {
            index_hint: "1\0",
            ..stamped()
        },
        Stamp {
            build_time: "x".repeat(LEN).leak(),
            ..stamped()
        },
    ] {
        assert!(encode(&stamp).is_err(), "{stamp:?}");
    }
}

#[test]
fn the_first_install_wins_and_unstamped_reads_until_then() {
    assert_eq!(installed(), None);
    assert_eq!(current(), &Stamp::UNSTAMPED);
    assert_eq!(app_version(), "0.0.0");
    let first = leak(encode(&stamped()).unwrap());
    install(first, Stamp::UNSTAMPED);
    install(
        leak(
            encode(&Stamp {
                app_version: "9.9.0",
                ..stamped()
            })
            .unwrap(),
        ),
        Stamp::UNSTAMPED,
    );
    let expected = Stamp {
        compiler_flavor: Stamp::UNSTAMPED.compiler_flavor,
        ..stamped()
    };
    assert_eq!(current(), &expected);
    assert_eq!(app_version(), "0.157.0");
    assert_eq!(build_id(), "0.157.0+g0123456789ab-dirty");
}

#[test]
fn a_release_is_the_versions_core_and_nothing_else_reads_as_one() {
    let of = |app_version: &'static str| {
        Stamp {
            app_version,
            ..Stamp::UNSTAMPED
        }
        .release()
    };
    assert_eq!(of("0.156.0"), Some((0, 156, 0)));
    assert_eq!(of("0.156.0+g0123456789ab"), Some((0, 156, 0)));
    assert_eq!(of("1.2.3-rc.1"), Some((1, 2, 3)));
    for refused in ["", "0.156", "0.156.0.1", "v0.156.0", "0.x.0", "0..0"] {
        assert_eq!(of(refused), None, "{refused}");
    }
}
