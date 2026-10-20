// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The tests of [`crate::capfile`], kept out of `capfile.rs` because `asb` and
//! `aspump` compile that file into themselves by path: in it, every one of these
//! would run again in each binary, over the same source under the same cfg.

use crate::capfile::*;

/// A tag whose bytes are `base, base+1, …`, so eight of them cover every byte
/// value once.
fn tag_from(base: u8) -> [u8; TAG_BYTES] {
    let mut tag = [0u8; TAG_BYTES];
    for (i, b) in tag.iter_mut().enumerate() {
        *b = base.wrapping_add(i as u8);
    }
    tag
}

fn hex_of(tag: &[u8; TAG_BYTES]) -> String {
    tag.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn debug_output_names_the_grant_but_never_the_tag() {
    let line = Line {
        grant: "ro:/f/F/>".to_string(),
        tag: tag_from(0xa0),
    };
    assert_eq!(
        format!("{line:?}"),
        r#"Line { grant: "ro:/f/F/>", tag: "<redacted>" }"#
    );
}

/// The tag is written as lowercase hex, high nibble first, every byte two
/// digits — pinned on bytes that exercise every nibble value in both halves.
#[test]
fn format_line_writes_the_tag_as_two_lowercase_hex_digits_per_byte() {
    let mut tag = [0u8; TAG_BYTES];
    for (i, b) in tag.iter_mut().enumerate() {
        *b = u8::try_from(i * 0x11 % 256).unwrap() ^ 0x0f;
    }
    tag[TAG_BYTES - 1] = 0xff;
    let line = format_line("ro:/f/>", &tag);
    let want: String = tag.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(line, format!("ro:/f/> {want}"));
    assert_eq!(parse_line(&line).unwrap().unwrap().tag, tag);
}

/// What a mint prints is what the reader reads — for a grant holding a space,
/// several spaces, or ENDING in one, since the tag is the whitespace-free tail.
#[test]
fn a_formatted_line_round_trips_even_when_the_grant_holds_a_space() {
    for grant in [
        "ro:/f/F/pub/>",
        "rw,p=n-a1b2c3d4e5f60718:/f/F/in/*/*/n-a1b2c3d4e5f60718/*",
        "ro:/f/F/pub a/>",
        "ro:/f/F/a  b/c d/>",
        "ro:/f/F/pub a/> ",
        "ro:/f/F/#tag/>",
    ] {
        for base in (0..=u8::MAX).step_by(TAG_BYTES) {
            let tag = tag_from(base);
            let line = format_line(grant, &tag);
            assert_eq!(
                line,
                format!("{grant} {}", hex_of(&tag)),
                "one space, lowercase"
            );
            assert_eq!(
                parse_line(&line),
                Ok(Some(Line {
                    grant: grant.to_string(),
                    tag
                })),
                "{line:?}"
            );
        }
    }
    // Uppercase hex is read too; only the writer is lowercase.
    let upper = format!("ro:/f/F/> {}", "AB".repeat(TAG_BYTES));
    assert_eq!(parse_line(&upper).unwrap().unwrap().tag, [0xab; TAG_BYTES]);
}

/// Blank and `#` lines are skipped, and ONLY those: a `#` inside a grant is a
/// grant, and a `#` after the tag is a malformed tag, not a trailing comment.
#[test]
fn blank_and_comment_lines_are_skipped_and_nothing_else_is() {
    let tag = hex_of(&tag_from(7));
    let commented_out = format!("#ro:/f/F/> {tag}");
    for skipped in [
        "",
        "   ",
        "\t \r",
        "#",
        "# the ring",
        "   # indented",
        commented_out.as_str(),
    ] {
        assert_eq!(parse_line(skipped), Ok(None), "{skipped:?}");
    }
    assert!(parse_line(&format!("ro:/f/#/> {tag}")).unwrap().is_some());
    assert_eq!(
        parse_line(&format!("ro:/f/F/> {tag} # note")),
        Err(Error::TagLength(4))
    );
    // Surrounding whitespace and a CR are trimmed, not read as grant or tag.
    let padded = parse_line(&format!("  ro:/f/F/>\t{tag}  \r"))
        .unwrap()
        .unwrap();
    assert_eq!(padded.grant, "ro:/f/F/>");
    assert_eq!(padded.tag, tag_from(7));
}

/// A grant with no tag, and a tag with no grant, are the same shape error — and
/// so is a line whose only separator is NON-ASCII whitespace: a mint writes one
/// ASCII space, and the separator is ASCII whitespace only.
#[test]
fn a_line_missing_either_half_is_a_shape_error() {
    let tag = hex_of(&tag_from(0));
    let indented = format!("  {tag}");
    let nbsp = format!("ro:/f/F/>\u{a0}{tag}");
    let ideographic = format!("ro:/f/F/>\u{3000}{tag}");
    for bad in [
        "ro:/f/F/>",
        "ro:/f/F/>   ",
        tag.as_str(),
        indented.as_str(),
        nbsp.as_str(),
        ideographic.as_str(),
    ] {
        assert_eq!(parse_line(bad), Err(Error::Shape), "{bad:?}");
    }
}

/// The tag is EXACTLY 32 bytes of strict hex. Every refusal is an error, never
/// a panic, and no error message echoes the tag it refused.
#[test]
fn the_tag_is_exactly_32_bytes_of_strict_hex() {
    let good = hex_of(&tag_from(0x40));
    for n in [2, 62, 63, 65, 66, 128] {
        let tag: String = good.chars().cycle().take(n).collect();
        let e = parse_line(&format!("ro:/f/F/> {tag}")).unwrap_err();
        assert_eq!(e, Error::TagLength(n), "{n} digits");
        assert!(
            !e.to_string().contains(&tag),
            "the refusal echoes the tag: {e}"
        );
    }
    let cases = [
        ("zz".repeat(TAG_BYTES), 0),
        (format!("0g{}", &good[2..]), 1),
        ("+f".repeat(TAG_BYTES), 0),
        ("-1".repeat(TAG_BYTES), 0),
        (format!("0x{}", &good[2..]), 1),
        // Multibyte: 64 BYTES of valid UTF-8, so the length check passes and
        // the decoder meets a byte of a character, not a char boundary.
        (format!("é{}", &good[2..]), 0),
        (format!("{}é{}", &good[..31], &good[33..]), 31),
        (format!("{}é{}", &good[..32], &good[34..]), 32),
        (format!("{}é", &good[..62]), 62),
    ];
    for (tag, at) in cases {
        assert_eq!(tag.len(), TAG_BYTES * 2, "{tag:?} is 64 bytes");
        let e = parse_line(&format!("ro:/f/F/> {tag}")).unwrap_err();
        assert_eq!(e, Error::TagNotHex(at), "{tag:?}");
        assert!(e.to_string().starts_with("tag: not valid hex"), "{e}");
    }
}

/// A ring reads in order, skipping blanks and comments; the first bad line
/// refuses the whole text and is named `path:line`, counting every line.
#[test]
fn parse_text_reads_a_ring_and_names_path_and_line() {
    let (a, b) = (tag_from(1), tag_from(2));
    let ring = format!(
        "# node ring\n\nro:/f/F/pub a/> {}\r\n  rw,p=n-a1:/f/F/in/n-a1/> {}\n",
        hex_of(&a),
        hex_of(&b)
    );
    assert_eq!(
        parse_text("node.cap", &ring),
        Ok(vec![
            Line {
                grant: "ro:/f/F/pub a/>".to_string(),
                tag: a
            },
            Line {
                grant: "rw,p=n-a1:/f/F/in/n-a1/>".to_string(),
                tag: b
            },
        ])
    );
    assert_eq!(parse_text("empty.cap", "\n# nothing\n"), Ok(vec![]));

    let bad = format!("{ring}# next\nro:/f/F/>\nro:/f/F/> zz\n");
    let e = parse_text("node.cap", &bad).unwrap_err();
    assert_eq!(
        e,
        Malformed {
            path: "node.cap".to_string(),
            line: 6,
            error: Error::Shape
        }
    );
    assert_eq!(e.to_string(), "node.cap:6: expected `<grant> <tag-hex>`");
}

/// The file reader: `path:line` for a malformed line, and a read error naming
/// the path — distinct from a malformed one — for a file it cannot read.
#[test]
fn read_file_names_path_and_line_and_keeps_a_read_error_distinct() {
    let dir = std::env::temp_dir().join(format!("astream-capfile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let tag = tag_from(9);

    let good = dir.join("good.cap");
    std::fs::write(
        &good,
        format!("# ring\n{}\n", format_line("ro:/f/F/ a/>", &tag)),
    )
    .unwrap();
    assert_eq!(
        read_file(&good).unwrap(),
        vec![Line {
            grant: "ro:/f/F/ a/>".to_string(),
            tag
        }]
    );

    let bad = dir.join("bad.cap");
    std::fs::write(&bad, format!("\nro:/f/F/> {}\n", "zz".repeat(TAG_BYTES))).unwrap();
    let e = read_file(&bad).unwrap_err();
    assert!(
        matches!(&e, ReadError::Malformed(m) if m.line == 2 && m.error == Error::TagNotHex(0)),
        "{e:?}"
    );
    assert_eq!(
        e.to_string(),
        format!("{}:2: tag: not valid hex at char 0", bad.display())
    );

    let missing = dir.join("missing.cap");
    let e = read_file(&missing).unwrap_err();
    assert!(matches!(&e, ReadError::Io { .. }), "{e:?}");
    assert!(
        e.to_string()
            .starts_with(&format!("{}: ", missing.display())),
        "{e}"
    );

    let binary = dir.join("binary.cap");
    std::fs::write(&binary, [b'r', b'o', 0xff, b'\n']).unwrap();
    assert!(matches!(read_file(&binary), Err(ReadError::Io { .. })));

    let _ = std::fs::remove_dir_all(&dir);
}
