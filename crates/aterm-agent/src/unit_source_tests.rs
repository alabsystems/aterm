// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn key_of(bytes: &[u8]) -> String {
    built::sha256_hex(bytes)
}

/// A store's staging folder for one ask, as targo makes it: `<store>/staging/<build>/<n>`, new,
/// private and reached through no link.
fn staging_in(root: &Path) -> PathBuf {
    let root = std::fs::canonicalize(root).unwrap();
    let staging = root.join("store/staging/build-1/1");
    std::fs::create_dir_all(&staging).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    staging
}

fn ask(version: u64, key: &str, staging: &Path) -> String {
    format!(
        "{{\"key\":\"{key}\",\"staging\":\"{}\",\"version\":{version}}}\n",
        staging.display()
    )
}

/// An ask is read only as targo sends it: version 1, a key, and a staging folder inside a
/// store's that is new (empty), private, this user's and reached through no link.
#[test]
fn an_ask_is_read_only_as_targo_sends_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let staging = staging_in(dir.path());
    let key = key_of(b"record");
    assert_eq!(
        parsed(ask(1, &key, &staging).as_bytes()),
        Some((key.clone(), staging.clone()))
    );
    assert_eq!(parsed(ask(2, &key, &staging).as_bytes()), None);
    assert_eq!(parsed(ask(1, "ABC", &staging).as_bytes()), None);
    assert_eq!(
        parsed(ask(1, &key, dir.path()).as_bytes()),
        None,
        "no store's staging"
    );
    assert_eq!(parsed(b"not json\n"), None);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            parsed(ask(1, &key, &staging).as_bytes()),
            None,
            "not private"
        );
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700)).unwrap();
        let linked = staging.parent().unwrap().join("2");
        std::os::unix::fs::symlink(&staging, &linked).unwrap();
        assert_eq!(parsed(ask(1, &key, &linked).as_bytes()), None, "a link");
    }
    std::fs::write(staging.join("planted"), b"").unwrap();
    assert_eq!(parsed(ask(1, &key, &staging).as_bytes()), None, "not empty");
}

/// Whom the source asks: nobody, at Full Access, with all the room it needs.
fn nobody() -> Here {
    Here {
        full: Box::new(|| true),
        admitted: Box::new(Vec::new),
        room: Box::new(|_, _| true),
    }
}

fn asks_of(text: String) -> Receiver<Vec<u8>> {
    lines(io::Cursor::new(text.into_bytes()))
}

/// Below Full Access, or with no member to ask, every ask is answered `none`, nothing else is
/// said (no progress is no `working`), and the asks are counted for the build's row.
#[test]
fn a_source_with_no_member_answers_none_to_each_ask() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let staging = staging_in(dir.path());
    let line = ask(1, &key_of(b"record"), &staging);
    for fleet in [Loading::Read(None), Loading::Read(Some(empty_fleet()))] {
        let mut output = Vec::new();
        let outcome = run(
            &asks_of(format!("{line}{line}")),
            &mut output,
            fleet,
            &nobody(),
        );
        assert_eq!(String::from_utf8(output).unwrap(), "none\nnone\n");
        assert_eq!(outcome.asked, 2);
        assert!(outcome.staged.is_empty() && outcome.refused.is_empty());
    }
}

/// What the source says, line by line, as it says it.
struct Said(mpsc::Sender<String>);

impl Write for Said {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for line in String::from_utf8_lossy(bytes).lines() {
            let _ = self.0.send(line.to_owned());
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The source reads its first ask before this Mac's fleet is read: while the read runs it says
/// `working`, and once it is read the ask is answered.
#[test]
fn the_first_ask_waits_for_the_fleet_read_while_saying_working() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let staging = staging_in(dir.path());
    let (read, fleet) = mpsc::channel();
    let (say, said) = mpsc::channel();
    // The fleet's read ends only once the source said it is working.
    let reader = std::thread::spawn(move || {
        let first = said.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        let _ = read.send(None);
        let mut lines: Vec<String> = first.into_iter().collect();
        lines.extend(said.iter());
        lines
    });
    let outcome = run(
        &asks_of(ask(1, &key_of(b"record"), &staging)),
        &mut Said(say),
        Loading::Reading(fleet),
        &nobody(),
    );
    let said = reader.join().unwrap();
    assert_eq!(
        said.first().map(String::as_str),
        Some("working"),
        "{said:?}"
    );
    assert_eq!(said.last().map(String::as_str), Some("none"), "{said:?}");
    assert!(
        said[..said.len() - 1].iter().all(|line| line == "working"),
        "{said:?}"
    );
    assert_eq!(outcome.asked, 1);
}

/// A fleet of this Mac alone: an identity and no member.
fn empty_fleet() -> Fleet {
    Fleet {
        me: (
            FleetSecret::new(&[7; 32]).expect("a secret"),
            NodeIdentity::from_seed(&"a".repeat(32), &[9; 32]).expect("an identity"),
        ),
        members: Vec::new(),
    }
}

fn landed(rel: &str, bytes: &[u8]) -> Landed {
    Landed {
        rel: rel.to_owned(),
        size: bytes.len() as u64,
        sha256: key_of(bytes),
    }
}

/// A unit is staged only where a record has the key and its one receipt names exactly the
/// outputs that landed (at their path in this store) with their size and digest; an `out/` file
/// the receipt does not name is not staged. No receipt, two, one naming another folder's
/// outputs or an output that did not land, or one an output differs from, refuses the unit.
#[test]
fn a_unit_is_held_to_its_key_and_its_receipt() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let staging = staging_in(dir.path());
    let store = store_of(&staging).unwrap().to_path_buf();
    let unit_dir = "build/c/0123";
    let rlib = b"!<arch>\nreal";
    let digest_in = |dir: &str| {
        built::sha256_hex(
            store
                .join(dir)
                .join("out/libc.rlib")
                .as_os_str()
                .as_encoded_bytes(),
        )
    };
    let receipt_of = |entries: &[(String, usize, String)]| {
        let outputs: Vec<String> = entries
            .iter()
            .map(|(at, size, sha)| {
                format!("{{\"path_sha256\":\"{at}\",\"size\":{size},\"sha256\":\"{sha}\"}}")
            })
            .collect();
        format!("{{\"outputs\":[{}]}}", outputs.join(","))
    };
    let good = receipt_of(&[(digest_in(unit_dir), rlib.len(), key_of(rlib))]);
    std::fs::create_dir_all(staging.join("fingerprint")).unwrap();
    let unit = |receipts: &[&str], extra: &[&str]| {
        let mut files = vec![landed("fingerprint/lib-c.json", b"record")];
        for (n, receipt) in receipts.iter().enumerate() {
            let rel = format!("fingerprint/outputs-lib-c{n}.json");
            std::fs::write(staging.join(&rel), receipt).unwrap();
            files.push(landed(&rel, receipt.as_bytes()));
        }
        files.push(landed("out/libc.rlib", rlib));
        for rel in extra {
            files.push(landed(rel, b"dep-info"));
        }
        Received {
            dir: unit_dir.to_owned(),
            files,
        }
    };
    let key = key_of(b"record");
    assert_eq!(
        judged(&unit(&[&good], &[]), &key, &store, &staging),
        Ok(Vec::new())
    );
    assert_eq!(
        judged(&unit(&[&good], &["out/libc.d"]), &key, &store, &staging),
        Ok(vec!["out/libc.d".to_owned()]),
        "an unlisted output is not staged"
    );
    let refused = [
        ("no receipt", unit(&[], &[])),
        ("two receipts", unit(&[&good, &good], &[])),
        (
            "another digest",
            unit(
                &[&receipt_of(&[(
                    digest_in(unit_dir),
                    rlib.len(),
                    key_of(b"other"),
                )])],
                &[],
            ),
        ),
        (
            "another folder",
            unit(
                &[&receipt_of(&[(
                    digest_in("build/c/9999"),
                    rlib.len(),
                    key_of(rlib),
                )])],
                &[],
            ),
        ),
        (
            "an output that did not land",
            unit(
                &[&receipt_of(&[
                    (digest_in(unit_dir), rlib.len(), key_of(rlib)),
                    (key_of(b"elsewhere"), 1, key_of(b"x")),
                ])],
                &[],
            ),
        ),
        ("no outputs", unit(&[&receipt_of(&[])], &[])),
        ("not a receipt", unit(&["not a receipt"], &[])),
    ];
    for (why, received) in refused {
        assert_eq!(
            judged(&received, &key, &store, &staging),
            Err(Refusal::Receipt),
            "{why}"
        );
    }
    assert_eq!(
        judged(&unit(&[&good], &[]), &key_of(b"another"), &store, &staging),
        Err(Refusal::Fingerprint)
    );
}
