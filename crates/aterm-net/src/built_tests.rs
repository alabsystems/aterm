// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
fn pair() -> (
    std::os::unix::net::UnixStream,
    std::os::unix::net::UnixStream,
) {
    std::os::unix::net::UnixStream::pair().expect("a socket pair")
}

fn key(n: u8) -> String {
    sha256_hex(&[n])
}

#[test]
fn keys_dirs_and_files_are_held_to_their_grammar() {
    assert!(is_key(&key(1)));
    assert!(!is_key(&key(1).to_uppercase()));
    assert!(!is_key("abc"));
    assert!(is_unit_dir("build/serde/0123abcd"));
    assert!(is_unit_dir("build/git/abc/serde/0123abcd"));
    for dir in [
        "build/serde",
        "build/../x/y",
        "out/serde/0123",
        "build/a/b/c",
        "build/a/.lock",
        "/build/a/b",
    ] {
        assert!(!is_unit_dir(dir), "{dir}");
    }
    assert!(is_unit_file("out/libserde-0123.rlib"));
    assert!(is_unit_file("fingerprint/lib-serde.json"));
    for file in [
        "out/../x",
        "out/a/b",
        "src/lib.rs",
        "fingerprint/.lock",
        "out/",
    ] {
        assert!(!is_unit_file(file), "{file}");
    }
}

/// Bounds that never cut a test's reads: a socket pair's peer is a thread of this test.
#[cfg(unix)]
fn open() -> (impl Fn(Duration) -> io::Result<()>, Instant) {
    (|_| Ok(()), Instant::now() + Duration::from_secs(3600))
}

#[cfg(unix)]
fn room(_: u64) -> bool {
    true
}

/// A member holding the unit under `wanted` at `unit`, serving one connection.
#[cfg(unix)]
fn holding(
    mut member: std::os::unix::net::UnixStream,
    unit: PathBuf,
    wanted: String,
) -> std::thread::JoinHandle<Served> {
    std::thread::spawn(move || {
        serve(&mut member, None, &mut |asked| {
            (asked == wanted).then(|| Held {
                dir: "build/a/0123".to_owned(),
                files: vec![
                    ("out/liba.rlib".to_owned(), unit.join("out/liba.rlib")),
                    (
                        "fingerprint/lib-a.json".to_owned(),
                        unit.join("fingerprint/lib-a.json"),
                    ),
                ],
                hold: Box::new(()),
            })
        })
        .unwrap()
    })
}

#[cfg(unix)]
#[test]
fn a_held_unit_lands_file_by_file_and_an_unheld_key_is_none() {
    let held = aterm_tempfile::tempdir().expect("a folder");
    let unit = held.path().join("unit");
    std::fs::create_dir_all(unit.join("out")).unwrap();
    std::fs::create_dir_all(unit.join("fingerprint")).unwrap();
    std::fs::write(unit.join("out/liba.rlib"), b"!<arch>\nbytes").unwrap();
    std::fs::write(unit.join("fingerprint/lib-a.json"), b"{}").unwrap();
    let wanted = key(7);
    let (mut asker, member) = pair();
    let server = holding(member, unit, wanted.clone());
    let staging = held.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    let (timeout, deadline) = open();
    let bounds = Bounds {
        timeout: &timeout,
        deadline,
    };
    assert!(!has(&mut asker, &key(8), &bounds).unwrap());
    assert!(has(&mut asker, &wanted, &bounds).unwrap());
    let none = get(&mut asker, &key(8), &staging, &bounds, &room, &mut |_| {}).unwrap();
    assert!(none.is_none());
    let mut moved = 0;
    let received = get(&mut asker, &wanted, &staging, &bounds, &room, &mut |n| {
        moved += n;
    })
    .unwrap()
    .expect("the unit");
    drop(asker);
    assert_eq!(received.dir, "build/a/0123");
    assert_eq!(received.files.len(), 2);
    assert_eq!(moved, 15);
    assert_eq!(
        std::fs::read(staging.join("out/liba.rlib")).unwrap(),
        b"!<arch>\nbytes"
    );
    let served = server.join().unwrap();
    assert_eq!((served.asked, served.units, served.bytes), (4, 1, 15));
    assert_eq!(served.ended, None);
}

/// A unit this Mac has no room for is not read: the asker gives up on the connection before a
/// byte lands, and the member is not at fault.
#[cfg(unix)]
#[test]
fn a_unit_with_no_room_here_lands_nothing() {
    let held = aterm_tempfile::tempdir().expect("a folder");
    let unit = held.path().join("unit");
    std::fs::create_dir_all(unit.join("out")).unwrap();
    std::fs::create_dir_all(unit.join("fingerprint")).unwrap();
    std::fs::write(unit.join("out/liba.rlib"), b"!<arch>\nbytes").unwrap();
    std::fs::write(unit.join("fingerprint/lib-a.json"), b"{}").unwrap();
    let (mut asker, member) = pair();
    let server = holding(member, unit, key(7));
    let staging = held.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    let (timeout, deadline) = open();
    let bounds = Bounds {
        timeout: &timeout,
        deadline,
    };
    let missed = get(
        &mut asker,
        &key(7),
        &staging,
        &bounds,
        &|bytes| bytes < 15,
        &mut |_| {},
    )
    .unwrap_err();
    assert!(matches!(missed, Missed::NoRoom(15)), "{missed}");
    drop(asker);
    let _ = server.join();
    assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
}

/// A member whose bytes do not match the SHA-256 it sent leaves nothing in the staging folder.
#[cfg(unix)]
#[test]
fn bytes_that_do_not_match_their_line_leave_nothing() {
    let dir = aterm_tempfile::tempdir().expect("a folder");
    let staging = dir.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    let (mut asker, mut member) = pair();
    let liar = std::thread::spawn(move || {
        let mut line = Vec::new();
        let mut byte = [0];
        while member.read(&mut byte).unwrap() == 1 && byte[0] != b'\n' {
            line.push(byte[0]);
        }
        let sent = format!(
            "unit build/a/0123 1 4\nfile out/liba.rlib 4 {}\nnope",
            sha256_hex(b"real")
        );
        member.write_all(sent.as_bytes()).unwrap();
    });
    let (timeout, deadline) = open();
    let bounds = Bounds {
        timeout: &timeout,
        deadline,
    };
    let missed = get(&mut asker, &key(1), &staging, &bounds, &room, &mut |_| {}).unwrap_err();
    liar.join().unwrap();
    assert!(matches!(missed, Missed::Transfer(_)), "{missed}");
    assert!(!staging.join("out/liba.rlib").exists());
}

/// A member that sends a unit's lines a byte at a time, each inside the read timeout, cannot
/// hold the ask past its deadline: the read ends there and what landed is removed.
#[cfg(unix)]
#[test]
fn a_member_that_trickles_is_cut_at_the_asks_deadline() {
    let dir = aterm_tempfile::tempdir().expect("a folder");
    let staging = dir.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    let (mut asker, mut member) = pair();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let trickler = std::thread::spawn({
        let stop = std::sync::Arc::clone(&stop);
        move || {
            let mut byte = [0];
            while member.read(&mut byte).unwrap_or(0) == 1 && byte[0] != b'\n' {}
            let head = format!(
                "unit build/a/0123 1 4\nfile out/liba.rlib 4 {}\n",
                sha256_hex(b"real")
            );
            for byte in head.bytes().chain(std::iter::repeat(b'r')) {
                if stop.load(std::sync::atomic::Ordering::Relaxed)
                    || member.write_all(&[byte]).is_err()
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    });
    let timeout = |_| Ok(());
    let bounds = Bounds {
        timeout: &timeout,
        deadline: Instant::now() + Duration::from_millis(100),
    };
    let missed = get(&mut asker, &key(1), &staging, &bounds, &room, &mut |_| {}).unwrap_err();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    drop(asker);
    trickler.join().unwrap();
    assert!(matches!(missed, Missed::Slow(_)), "{missed}");
    assert!(!staging.join("out/liba.rlib").exists());
}

/// A member that turned sharing off refuses the first ask and ends the connection; an ask out of
/// the grammar (no verb, or a key that is not one) is refused the same way.
#[cfg(unix)]
#[test]
fn a_refusing_member_answers_err() {
    let dir = aterm_tempfile::tempdir().expect("a folder");
    let (mut asker, mut member) = pair();
    let server = std::thread::spawn(move || {
        serve(&mut member, Some("sharing is off"), &mut |_| {
            panic!("an off member looks nothing up")
        })
        .unwrap()
    });
    let (timeout, deadline) = open();
    let bounds = Bounds {
        timeout: &timeout,
        deadline,
    };
    let missed = get(&mut asker, &key(1), dir.path(), &bounds, &room, &mut |_| {}).unwrap_err();
    assert!(matches!(missed, Missed::Refused(ref why) if why == "sharing is off"));
    assert_eq!(server.join().unwrap().asked, 0);

    for ask in [key(1), "has ABC".to_owned(), format!("put {}", key(1))] {
        let (mut asker, mut member) = pair();
        let server = std::thread::spawn(move || {
            serve(&mut member, None, &mut |_| panic!("no key is looked up")).unwrap()
        });
        asker.write_all(format!("{ask}\n").as_bytes()).unwrap();
        let mut answer = String::new();
        asker.read_to_string(&mut answer).unwrap();
        assert_eq!(answer, "err not an ask\n", "{ask}");
        assert_eq!(server.join().unwrap().asked, 0, "{ask}");
    }
}
