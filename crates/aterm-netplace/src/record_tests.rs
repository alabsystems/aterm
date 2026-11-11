// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn historical() -> Written {
    let id = NetworkId::parse("n0123456789abcdef").expect("an id");
    Written {
        revision: 7,
        keeper_pid: Some(4242),
        keeper_started_ms: Some(1_000),
        written_ms: 2_000,
        record: Record {
            stays: vec![
                Stay {
                    start_ms: 1_000,
                    end_ms: Some(1_500),
                    at: StayAt::AlabLan,
                    network: Some(id.clone()),
                    subnet: Some("192.168.4.0/22".to_owned()),
                    gateway: Some(Ipv4Addr::new(192, 168, 4, 1)),
                    from_start: true,
                },
                Stay {
                    start_ms: 1_500,
                    end_ms: None,
                    at: StayAt::Offline,
                    network: None,
                    subnet: None,
                    gateway: None,
                    from_start: false,
                },
            ],
            remembered: vec![Known {
                network: id,
                learned_ms: 1_100,
                confirmed_ms: 1_400,
            }],
        },
    }
}

/// What the keeper writes it reads back whole, and a record of another schema or with one
/// line it cannot read is unread as a whole, so no half-read memory invents a time.
/// NEGATIVE CONTROL: passing over a key this build does not know (`_ => {}` in `parse`)
/// reads the record with the unknown key, and this test fails.
#[test]
fn the_record_reads_back_and_an_unknown_one_is_unread_whole() {
    let written = historical();
    let text = format(&written);
    assert_eq!(parse(&text), Some(written));
    assert_eq!(parse(&text.replace("schema = 1", "schema = 2")), None);
    assert_eq!(parse(&text.replace("alab-lan", "somewhere")), None);
    assert_eq!(
        parse(&text.replace("revision = 7", "revision = 7\nmystery = 1")),
        None,
        "a key this build does not know"
    );
    assert_eq!(
        parse(&text.replace("n0123456789abcdef 1100", "w0123456789abcdef 1100")),
        None,
        "a weak id is never remembered"
    );
    assert_eq!(parse(""), None);
}

#[test]
fn schema_one_memory_survives_without_retaining_its_retired_path() {
    let legacy = "path = \"usable=0 expensive=1 constrained=0 link=wifi\"\n\
        revision = 7\nkeeper_pid = 4242\nkeeper_started_ms = 1000\nwritten_ms = 2000\n\
        stays = [\n\"1000 1500 alab-lan n0123456789abcdef 192.168.4.0/22 192.168.4.1 1\",\n\
        \"1500 - offline - - - 0\",\n]\nschema = 1\n\
        remembered = [\n\"n0123456789abcdef 1100 1400\",\n]\n";
    let expected = historical();
    assert_eq!(parse(legacy), Some(expected.clone()));
    let without_path = legacy.lines().skip(1).collect::<Vec<_>>().join("\n");
    assert_eq!(parse(&without_path), Some(expected.clone()));
    for malformed in [
        legacy.replace("expensive=1", "expensive=yes"),
        legacy.replace("constrained=0", "constrained=2"),
        legacy.replace("usable=0", "usable=0 usable=1"),
        legacy.replace("link=wifi", "link=unknown-link"),
        format!("{legacy}path = \"usable=0 expensive=1 constrained=0 link=wifi\"\n"),
        legacy.replace("schema = 1", "schema = 2"),
        legacy.replace("schema = 1", "schema = 3"),
    ] {
        assert_eq!(parse(&malformed), None);
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir(root.path());
    aterm_uds::private_file::ensure_private_dir(&dir).unwrap();
    aterm_uds::private_file::write_private(&dir, FILE, legacy.as_bytes()).unwrap();
    assert_eq!(read(&dir), Some(expected.clone()));
    assert_eq!(
        std::fs::read(dir.join(FILE)).unwrap(),
        legacy.as_bytes(),
        "reading does not force a rewrite"
    );
    write(&dir, &expected).unwrap();
    let rewritten = std::fs::read_to_string(dir.join(FILE)).unwrap();
    assert!(rewritten.lines().any(|line| line == "schema = 1"));
    assert!(!rewritten.lines().any(|line| line.starts_with("path = ")));
    assert_eq!(read(&dir), Some(expected));
}

/// Truncated files, duplicate keys and invalid CIDR `subnet`
/// tokens make the whole record unread rather than inventing defaults or overwriting fields.
/// NEGATIVE CONTROL: defaulting missing `revision`/`written_ms`/`stays`/`remembered` or
/// accepting an invalid CIDR accepts corrupted `place.toml` inputs, failing this test.
#[test]
fn truncated_duplicate_and_malformed_record_fields_are_unread_whole() {
    let base = format(&Written {
        revision: 3,
        written_ms: 9_000,
        record: Record {
            stays: vec![Stay {
                start_ms: 1_000,
                end_ms: None,
                at: StayAt::AlabLan,
                network: None,
                subnet: Some("10.0.0.0/24".to_owned()),
                gateway: Some(Ipv4Addr::new(10, 0, 0, 1)),
                from_start: true,
            }],
            remembered: Vec::new(),
        },
        ..Written::default()
    });
    assert!(parse(&base).is_some());
    assert_eq!(parse("schema = 1\n"), None);
    assert_eq!(parse("schema = 1\nrevision = 3\nwritten_ms = 9000\n"), None);
    assert_eq!(
        parse(&base.replace("revision = 3", "revision = 3\nrevision = 4")),
        None
    );
    assert_eq!(
        parse(&base.replace("stays = [", "stays = [\n]\nstays = [")),
        None
    );
    assert_eq!(parse(&base.replace("10.0.0.0/24", "10.0.0.0/33")), None);
    assert_eq!(parse(&base.replace("10.0.0.0/24", "not-a-cidr")), None);
}

/// The record is written private and read back through the private-file rules.
/// NEGATIVE CONTROL: writing it with a plain `std::fs::write` (umask mode) leaves it
/// unreadable through the private-file rules, and this test fails.
#[test]
fn the_record_is_written_private_and_read_back() {
    let root = aterm_tempfile::tempdir().expect("a scratch directory");
    let dir = dir(root.path());
    assert_eq!(read(&dir), None, "nothing written yet");
    let written = Written {
        revision: 1,
        written_ms: 5,
        ..Written::default()
    };
    write(&dir, &written).expect("written");
    assert_eq!(read(&dir), Some(written));
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(dir.join(FILE))
        .expect("the file")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let mode = std::fs::metadata(&dir)
        .expect("the dir")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
}
