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

// ---------------------------------------------------------------------------
// The secret-file rule: check_private, read_secret_file, SECRET_MIN
// ---------------------------------------------------------------------------

struct SecretScratch(std::path::PathBuf);

impl SecretScratch {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for SecretScratch {
    fn drop(&mut self) {
        // Only a directory this guard exclusively created reaches cleanup.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Allocate a private root without adopting or removing any preexisting entry.
fn secret_scratch(name: &str) -> SecretScratch {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut private_builder = builder;
        private_builder.mode(0o700);
        private_builder
    };
    for _ in 0..1024 {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("astream-secret-{name}-{}-{n}", std::process::id()));
        match builder.create(&dir) {
            Ok(()) => return SecretScratch(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => panic!("create owned secret scratch: {e}"),
        }
    }
    panic!("could not allocate an exclusive secret scratch name");
}

#[cfg(unix)]
fn chmod(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// 0600 or tighter passes; any group/other bit is refused, as `PermissionDenied`
/// naming the mode and the `chmod`, path first, by EACH of the three functions: the
/// check and both readers hold the same clause on the same open, so none of them
/// may answer a mode refusal in its own words. The passing modes are the control:
/// the same file, the same bytes, only the mode moved, and both readers then return
/// those bytes.
#[cfg(unix)]
#[test]
fn secret_file_other_users_can_read_is_refused_naming_the_chmod() {
    let scratch = secret_scratch("priv");
    let dir = scratch.path();
    let path = dir.join("k");
    let shown = path.display().to_string();
    let bytes = [0x5a; SECRET_MIN];
    std::fs::write(&path, bytes).unwrap();
    for (mode, ok) in [
        (0o600, true),
        (0o400, true),
        (0o700, true),
        (0o640, false),
        (0o604, false),
        (0o644, false),
        (0o660, false),
    ] {
        chmod(&path, mode);
        let checked = check_private(&path);
        let private = read_private_file(&path);
        let secret = read_secret_file(&path);
        if ok {
            assert!(checked.is_ok(), "{mode:o}: {checked:?}");
            assert_eq!(private.expect("owner-only"), bytes, "{mode:o}");
            assert_eq!(secret.expect("owner-only"), bytes, "{mode:o}");
            continue;
        }
        for (door, e) in [
            ("check_private", checked.expect_err("group/other bit")),
            ("read_private_file", private.expect_err("group/other bit")),
            ("read_secret_file", secret.expect_err("group/other bit")),
        ] {
            assert_eq!(
                e.kind(),
                std::io::ErrorKind::PermissionDenied,
                "{door} {mode:o}"
            );
            let text = e.to_string();
            assert!(
                text.starts_with(&format!("{shown} is mode {mode:04o}: ")),
                "{door}: {text}"
            );
            assert!(
                text.contains(&format!("chmod 600 {shown}")),
                "{door}: {text}"
            );
        }
    }
}

/// `read_private_file` is the owner-only rule WITHOUT the length floor: the bytes
/// of a file shorter than a secret come back exactly, where `read_secret_file`
/// refuses the same file. That is the reader for a file whose caller decodes it (a
/// hex key), and the control that the floor is `read_secret_file`'s alone.
#[cfg(unix)]
#[test]
fn secret_file_private_reader_has_no_length_floor() {
    let scratch = secret_scratch("plain");
    let dir = scratch.path();
    let path = dir.join("k.hex");
    let hex = format!("{}\n", "ab".repeat(8));
    std::fs::write(&path, &hex).unwrap();
    chmod(&path, 0o600);
    assert_eq!(
        read_private_file(&path).expect("owner-only"),
        hex.as_bytes()
    );
    assert_eq!(
        read_secret_file(&path).expect_err("17 bytes").kind(),
        std::io::ErrorKind::InvalidData
    );
    std::fs::write(&path, b"").unwrap();
    assert_eq!(read_private_file(&path).expect("empty"), b"");
}

/// A secret is owner-only and at least 32 bytes, or refused with the reason by kind,
/// and is read byte for byte: a trailing newline is part of it, and so are 32 bytes
/// of which the last is a space (what `head -c 32 /dev/urandom` writes about one
/// time in twenty-five). Controls: 31 bytes is refused where 32 is not, and the
/// newline-terminated secret is NOT its trimmed text.
#[cfg(unix)]
#[test]
fn secret_file_is_owner_only_long_enough_and_read_byte_for_byte() {
    let scratch = secret_scratch("mint");
    let dir = scratch.path();
    let path = dir.join("mint.secret");
    assert_eq!(
        read_secret_file(&path).expect_err("absent").kind(),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(
        check_private(&path).expect_err("absent").kind(),
        std::io::ErrorKind::NotFound
    );

    let mut bytes = vec![0x5a; 31];
    bytes.push(b'\n');
    std::fs::write(&path, &bytes).unwrap();
    chmod(&path, 0o644);
    let e = read_secret_file(&path).expect_err("0644");
    assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
    chmod(&path, 0o600);
    let got = read_secret_file(&path).expect("0600, 32 bytes");
    assert_eq!(got, bytes);
    assert_ne!(got, bytes.trim_ascii());

    let mut spaced = vec![b'k'; 31];
    spaced.push(b' ');
    std::fs::write(&path, &spaced).unwrap();
    assert_eq!(
        read_secret_file(&path).expect("32, the last a space"),
        spaced
    );

    std::fs::write(&path, [0x5a; SECRET_MIN - 1]).unwrap();
    let e = read_secret_file(&path).expect_err("short");
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    let shown = path.display().to_string();
    assert_eq!(
        e.to_string(),
        format!(
            "{shown} holds 31 bytes; a secret file holds at least 32 (a short key mints \
             capabilities that seal nothing)"
        )
    );
    std::fs::write(&path, b"").unwrap();
    assert_eq!(
        read_secret_file(&path).expect_err("empty").kind(),
        std::io::ErrorKind::InvalidData
    );
}

/// Every refusal names the path FIRST and an OS error names it ONCE: the OS never
/// names the path it failed on, so the reader re-wraps the error as `<path>: …`
/// with its kind kept, and a caller that prefixes only its flag (`--secret-file
/// {e}`) prints the path exactly once. Negative control: the raw OS error for the
/// same missing file does NOT name the path, so the wrap is what does.
#[test]
fn secret_file_refusals_name_the_path_first_and_keep_the_kind() {
    let scratch = secret_scratch("path");
    let dir = scratch.path();
    let missing = dir.join("absent.secret");
    let shown = missing.display().to_string();
    for e in [
        check_private(&missing).expect_err("absent"),
        read_private_file(&missing).expect_err("absent"),
        read_secret_file(&missing).expect_err("absent"),
    ] {
        assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
        let text = e.to_string();
        assert!(text.starts_with(&format!("{shown}: ")), "{text}");
        assert_eq!(text.matches(&shown).count(), 1, "{text}");
    }
    let raw = std::fs::File::open(&missing).expect_err("absent");
    assert!(!raw.to_string().contains(&shown), "{raw}");

    // An owner-only directory opens on Unix and fails at the read (EISDIR): an OS
    // error from the read, not the open, still path-first, the path once, and of
    // the kind the raw read of the same directory fails with (the control: the
    // wrap keeps the OS's kind rather than choosing one of its own).
    #[cfg(unix)]
    {
        use std::io::Read as _;
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        chmod(&sub, 0o700);
        let sub_shown = sub.display().to_string();
        let raw_kind = std::fs::File::open(&sub)
            .expect("a directory opens read-only")
            .read_to_end(&mut Vec::new())
            .expect_err("a directory does not read")
            .kind();
        assert!(
            !matches!(
                raw_kind,
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidData
            ),
            "{raw_kind:?}"
        );
        for e in [
            read_private_file(&sub).expect_err("a directory is not a secret"),
            read_secret_file(&sub).expect_err("a directory is not a secret"),
        ] {
            assert_eq!(e.kind(), raw_kind, "{e}");
            let text = e.to_string();
            assert!(text.starts_with(&format!("{sub_shown}: ")), "{text}");
            assert_eq!(text.matches(&sub_shown).count(), 1, "{text}");
            assert!(!text.contains("is mode"), "the mode passed: {text}");
        }
        assert!(check_private(&sub).is_ok(), "the mode alone passes");
    }
}

/// A read error discards authority, including the prefix read before the failure;
/// a successful read keeps those exact bytes, including its trailing newline.
#[test]
fn a_failed_secret_read_wipes_the_bytes_it_already_collected() {
    struct PartialError<'a>(&'a [u8]);
    impl std::io::Read for PartialError<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.0.is_empty() {
                return Err(std::io::Error::other("captured read failure"));
            }
            let n = out.len().min(self.0.len());
            out[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }
    let prefix = [0xa5; SECRET_MIN];
    let mut secret = Vec::new();
    let error = read_secret_into(&mut PartialError(&prefix), &mut secret).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Other);
    assert_eq!(error.to_string(), "captured read failure");
    assert_eq!(secret, vec![0; prefix.len()]);

    let mut exact = [0x5a; SECRET_MIN];
    exact[SECRET_MIN - 1] = b'\n';
    let mut secret = Vec::new();
    read_secret_into(&mut std::io::Cursor::new(exact), &mut secret).unwrap();
    assert_eq!(secret, exact);
}
