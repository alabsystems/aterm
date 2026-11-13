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

#[test]
fn windows_numeric_identity_uses_the_build_and_preserves_the_public_string() {
    let stamp = Stamp {
        app_version: "1.20261110.999999999",
        build_number: "65535",
        ..stamped()
    };
    assert_eq!(stamp.windows_numeric_version(), Ok([1, 0, 0, 65535]));
    let read = decode(leak(encode(&stamp).unwrap()), Stamp::UNSTAMPED).unwrap();
    assert_eq!(read.app_version, "1.20261110.999999999");
    assert_eq!(read.build_number, "65535");
    let next = Stamp {
        build_number: "65536",
        ..stamp
    };
    assert_eq!(next.windows_numeric_version(), Ok([1, 0, 1, 0]));
    assert!(next.windows_numeric_version().unwrap() > stamp.windows_numeric_version().unwrap());
    assert!(stamp.windows_numeric_version().unwrap() > [0, 65535, 65535, 0]);
}

#[test]
fn windows_numeric_identity_refuses_lossy_or_malformed_builds() {
    let maximum = Stamp {
        build_number: "281474976710655",
        ..stamped()
    };
    assert_eq!(
        maximum.windows_numeric_version(),
        Ok([1, 65535, 65535, 65535])
    );
    for build_number in [
        "",
        "+1",
        "-1",
        "1x",
        "281474976710656",
        "18446744073709551616",
    ] {
        assert!(
            Stamp {
                build_number,
                ..maximum
            }
            .windows_numeric_version()
            .is_err()
        );
    }
}

#[test]
fn development_tag_selection_uses_product_admission() {
    let args = describe_release_args("v0.197.0\nv1.20240229.1\nv1.20260231.1\nv1.20261110.0\nv0.197.1\nv1.20261110.1000000000\nv1.20261110.1-rc1\n").unwrap();
    assert_eq!(
        args,
        [
            "describe",
            "--tags",
            "--abbrev=0",
            "--match",
            "v0.197.0",
            "--match",
            "v1.20240229.1"
        ]
    );
    assert!(describe_release_args("v1.20261110.0\nv1.20260231.1\n").is_none());
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

#[test]
fn the_unstamped_record_is_what_encode_writes_for_unstamped() {
    assert_eq!(UNSTAMPED_RECORD, encode(&Stamp::UNSTAMPED).unwrap());
    assert_eq!(
        decode(leak(UNSTAMPED_RECORD), Stamp::UNSTAMPED).unwrap(),
        Stamp::UNSTAMPED
    );
}

#[test]
fn the_writer_replaces_the_one_record_an_image_holds() {
    // An image: code bytes, a lone header string (as the decoder's own literal compiles in),
    // the compiled-in record, more bytes.
    let mut image = b"\0asm code ".to_vec();
    image.extend_from_slice(HEADER.as_bytes());
    image.extend_from_slice(b"version=1.0.0\nnot a record");
    let at = image.len();
    image.extend_from_slice(&UNSTAMPED_RECORD);
    image.extend_from_slice(b" tail");
    let before = image.clone();
    rewrite(&mut image, &stamped()).unwrap();
    assert_eq!(image.len(), before.len());
    assert_eq!(image[..at], before[..at], "the bytes before the record");
    assert_eq!(image[at + LEN..], before[at + LEN..], "the bytes after it");
    let record: [u8; LEN] = image[at..at + LEN].try_into().unwrap();
    assert_eq!(record, encode(&stamped()).unwrap());
    // A stamped image is stamped again the same way.
    let again = Stamp {
        app_version: "0.158.0",
        ..stamped()
    };
    rewrite(&mut image, &again).unwrap();
    assert_eq!(
        decode(
            leak(image[at..at + LEN].try_into().unwrap()),
            Stamp::UNSTAMPED
        )
        .unwrap()
        .app_version,
        "0.158.0"
    );

    // No record, two records, a record cut short by the image's end, or a stamp that does not
    // encode: refused, the image unchanged.
    let mut two = before.clone();
    two.extend_from_slice(&UNSTAMPED_RECORD);
    let mut cut = b"head ".to_vec();
    cut.extend_from_slice(&UNSTAMPED_RECORD[..LEN - 1]);
    let bad = Stamp {
        app_version: "0.1\n",
        ..Stamp::UNSTAMPED
    };
    for (mut image, stamp, why) in [
        (b"\0asm no record".to_vec(), stamped(), "no identity record"),
        (two, stamped(), "2 identity records"),
        (cut, stamped(), "no identity record"),
        (before.clone(), bad, "holds a newline"),
    ] {
        let kept = image.clone();
        let refused = rewrite(&mut image, &stamp).expect_err(why);
        assert!(refused.contains(why), "{why}: {refused}");
        assert_eq!(image, kept, "{why}");
    }
}
