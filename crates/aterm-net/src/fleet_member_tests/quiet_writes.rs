// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A contact that moves nothing but when a member was last heard, or a push that says the facts
//! its record holds, replaces no book and no record.

use super::*;
use std::os::unix::fs::MetadataExt as _;

const T0: u64 = 1_760_000_000_000;

/// The file `name` in `dir` as it is on disk: its inode, which every whole write replaces, and
/// its bytes.
fn on_disk(dir: &Path, name: &str) -> (u64, Vec<u8>) {
    let path = dir.join(name);
    (
        std::fs::symlink_metadata(&path).unwrap().ino(),
        std::fs::read(&path).unwrap(),
    )
}

fn seen_text(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join(SEEN_FILE)).ok()
}

fn seen_of(dir: &Path, node: u8) -> Option<u64> {
    read_peers(dir)
        .unwrap()
        .into_iter()
        .find(|peer| peer.node == Some(node_of(node)))
        .and_then(|peer| peer.seen_ms)
}

fn hear(dir: &Path, node: u8, at: u64, pace: seen::Pace) {
    hold_book(dir)
        .unwrap()
        .update_at(
            |book| {
                let peer = book
                    .iter_mut()
                    .find(|peer| peer.node == Some(node_of(node)))
                    .ok_or("no such member")?;
                peer.seen_ms = Some(at);
                Ok(())
            },
            pace,
        )
        .unwrap();
}

/// What a later `seen_ms` alone may put in the seen file, and what must write the book.
/// NEGATIVE CONTROL: have `advances` pass an earlier time (`new != old`): the fourth case fails.
#[test]
fn only_a_later_time_of_an_entry_that_had_one_stands_in_for_a_book_write() {
    let a = joined(0xa1, "a.local", [10, 0, 0, 1], 7100);
    let b = joined(0xb2, "b.local", [10, 0, 0, 2], 7100);
    let later = |peer: &Peer, by: u64| Peer {
        seen_ms: peer.seen_ms.map(|seen| seen + by),
        ..peer.clone()
    };
    let book = [a.clone(), b.clone()];
    assert_eq!(seen::advances(&book, &book), Some(Vec::new()));
    assert_eq!(
        seen::advances(&book, &[a.clone(), later(&b, 5)]),
        Some(vec![(format!("node={}", node_of(0xb2)), T0 + 5)])
    );
    let unheard = Peer {
        seen_ms: None,
        ..b.clone()
    };
    assert_eq!(
        seen::advances(&[a.clone(), unheard.clone()], &book),
        None,
        "a first time is the book's"
    );
    assert_eq!(
        seen::advances(&[a.clone(), later(&b, 5)], &book),
        None,
        "an earlier time is the book's"
    );
    assert_eq!(seen::advances(&book, &[a.clone(), unheard]), None);
    let moved = Peer {
        addr: Some(IpAddr::from([10, 0, 0, 9])),
        ..later(&b, 5)
    };
    assert_eq!(
        seen::advances(&book, &[a.clone(), moved]),
        None,
        "another field with it"
    );
    assert_eq!(seen::advances(&book, std::slice::from_ref(&a)), None);
    assert_eq!(seen::advances(&book, &[b.clone(), a.clone()]), None);
    assert_eq!(
        seen::advances(&[a.clone(), a.clone()], &[a.clone(), later(&a, 5)]),
        None,
        "a key two entries share names neither"
    );
}

/// A member heard again writes its time beside the book, which keeps its inode and bytes; every
/// read, as another process makes it from the files alone, has the later time; the next whole
/// write carries it and drops the line; and a time a whole write sets earlier stands.
/// NEGATIVE CONTROL: have `HeldBook::update_at` skip `seen::advances`: the book's inode moves.
/// Or drop `seen::settle` from it: the earlier time reads as the later one.
#[test]
fn a_member_heard_again_writes_its_time_beside_the_book_never_the_book() {
    let state = State::new("seen-beside");
    let dir = state.dir();
    update_peers(dir, |book| {
        record(book, joined(0xa1, "a.local", [10, 0, 0, 1], 7100))?;
        record(book, joined(0xb2, "b.local", [10, 0, 0, 2], 7100)).map(|_| ())
    })
    .unwrap();
    assert_eq!(seen_text(dir), None, "a first time is the book's own");
    let written = on_disk(dir, PEERS_FILE);

    hear(dir, 0xb2, T0 + 180_000, seen::Pace::Now);
    assert_eq!(on_disk(dir, PEERS_FILE), written, "the book is not written");
    let text = seen_text(dir).expect("the time is on disk");
    assert!(
        text.contains(&format!(
            "seen node={} ms={}\n",
            node_of(0xb2),
            T0 + 180_000
        )),
        "{text}"
    );
    assert_eq!(seen_of(dir, 0xb2), Some(T0 + 180_000));
    assert_eq!(seen_of(dir, 0xa1), Some(T0));
    // The same time again changes nothing on disk.
    let beside = on_disk(dir, SEEN_FILE);
    hear(dir, 0xb2, T0 + 180_000, seen::Pace::Now);
    assert_eq!(on_disk(dir, SEEN_FILE), beside);

    // A whole write (a member recorded) carries the later time into the book and drops the line.
    update_peers(dir, |book| {
        record(book, joined(0xc3, "c.local", [10, 0, 0, 3], 7100)).map(|_| ())
    })
    .unwrap();
    assert_ne!(on_disk(dir, PEERS_FILE).0, written.0);
    assert_eq!(seen_text(dir), None);
    let (book, _) = read_book_as_written(dir, None).unwrap();
    assert_eq!(
        book.iter()
            .map(|peer| peer.seen_ms.unwrap() - T0)
            .collect::<Vec<_>>(),
        [0, 180_000, 0],
        "the book's own lines hold what the seen file held"
    );

    // An earlier time is a whole write, and stands over the line a later time left.
    hear(dir, 0xa1, T0 + 500, seen::Pace::Now);
    assert!(seen_text(dir).is_some());
    hear(dir, 0xa1, T0 + 100, seen::Pace::Now);
    assert_eq!(seen_of(dir, 0xa1), Some(T0 + 100));
    assert_eq!(seen_text(dir), None);
}

/// A process that stays writes the first time it notes at once and holds the next in memory,
/// where its own reads find them, until [`SEEN_FLUSH_EVERY`] has passed since its last write.
/// NEGATIVE CONTROL: have `seen::note` read every `Pace::Within` as due: the second time is on
/// disk at once. Or have `seen::flush_late` skip its `due` test: the write one millisecond early
/// is made. Or have `seen::times` pass over the pending times: this process reads the old time.
#[test]
fn a_staying_process_writes_noted_times_once_a_pace_and_reads_them_at_once() {
    let state = State::new("seen-paced");
    let dir = state.dir();
    update_peers(dir, |book| {
        record(book, joined(0xa1, "a.local", [10, 0, 0, 1], 7100))?;
        record(book, joined(0xb2, "b.local", [10, 0, 0, 2], 7100)).map(|_| ())
    })
    .unwrap();
    let written = on_disk(dir, PEERS_FILE);
    let began = Instant::now();
    let line = |node: u8, by: u64| format!("seen node={} ms={}\n", node_of(node), T0 + by);

    hear(dir, 0xa1, T0 + 1_000, seen::Pace::Within(began));
    let first = seen_text(dir).expect("the first time noted is written at once");
    assert!(first.contains(&line(0xa1, 1_000)), "{first}");
    let beside = on_disk(dir, SEEN_FILE);

    let soon = began + Duration::from_secs(10);
    hear(dir, 0xb2, T0 + 11_000, seen::Pace::Within(soon));
    hear(dir, 0xa1, T0 + 12_000, seen::Pace::Within(soon));
    assert_eq!(
        on_disk(dir, SEEN_FILE),
        beside,
        "nothing more is written yet"
    );
    assert_eq!(on_disk(dir, PEERS_FILE), written);
    assert_eq!(
        seen_of(dir, 0xb2),
        Some(T0 + 11_000),
        "this process reads it"
    );
    assert_eq!(seen_of(dir, 0xa1), Some(T0 + 12_000));

    let early = began + SEEN_FLUSH_EVERY - Duration::from_millis(1);
    assert!(!seen::flush_late(dir, early).unwrap());
    assert_eq!(on_disk(dir, SEEN_FILE), beside);
    assert!(seen::flush_late(dir, began + SEEN_FLUSH_EVERY).unwrap());
    let text = seen_text(dir).unwrap();
    assert!(
        text.contains(&line(0xb2, 11_000)) && text.contains(&line(0xa1, 12_000)),
        "{text}"
    );
    assert!(
        !text.contains(&line(0xa1, 1_000)),
        "one line a member: {text}"
    );
    assert_eq!(
        on_disk(dir, PEERS_FILE),
        written,
        "the book was never written"
    );
    assert!(
        !seen::flush_late(dir, began + 2 * SEEN_FLUSH_EVERY).unwrap(),
        "nothing is noted"
    );

    // A time noted for a member another writer took out of the book meanwhile is dropped at the
    // next write, with every line the book's own times have caught up with: no file is left.
    let late = began + SEEN_FLUSH_EVERY + Duration::from_secs(10);
    hear(dir, 0xb2, T0 + 70_000, seen::Pace::Within(late));
    write_book(
        dir,
        &[Peer {
            seen_ms: Some(T0 + 12_000),
            ..joined(0xa1, "a.local", [10, 0, 0, 1], 7100)
        }],
    );
    assert!(seen::flush_late(dir, began + 2 * SEEN_FLUSH_EVERY).unwrap());
    assert_eq!(seen_text(dir), None);
}

/// A served registration of a member the book holds, later, replaces no book while the time
/// it brings is within [`SEEN_BEHIND_MS`] of the one the book's file has.
/// NEGATIVE CONTROL: have `HeldBook::update_at` skip `seen::advances`: the book's inode moves.
#[test]
fn a_registration_again_from_a_member_the_book_holds_replaces_no_book() {
    let state = State::new("seen-join");
    let dir = state.dir();
    write_key(dir, &[5; SECRET_LEN]).unwrap();
    let who = identity(&node_of(0xd4), 0x44);
    let register = |now_ms: u64| {
        let reg = Registration {
            node: who.node().to_owned(),
            host: "lab-mini".to_owned(),
            port: 58054,
            version: "0.146.0".to_owned(),
            key: None,
            signed: None,
        }
        .signed_by(&who, T0);
        let mut stream = Duplex {
            input: std::io::Cursor::new(reg.line().into_bytes()),
            output: Vec::new(),
        };
        serve_join_with_denied(
            &mut stream,
            (
                IpAddr::from(Ipv4Addr::LOCALHOST),
                Some(&proof(who.node(), who.key())),
            ),
            (dir, &secret(5)),
            || Ok(issuer(0x12)),
            now_ms,
            (&[], None),
            &|| true,
        )
        .expect("the stream reads and writes")
        .expect("the member is recorded")
        .0
    };
    assert_eq!(register(T0 + 1_000), Recorded::New);
    let written = on_disk(dir, PEERS_FILE);
    assert_eq!(register(T0 + 181_000), Recorded::Updated);
    assert_eq!(register(T0 + 271_000), Recorded::Updated);
    assert_eq!(on_disk(dir, PEERS_FILE), written);
    assert_eq!(seen_of(dir, 0xd4), Some(T0 + 271_000));
}

/// The book's own time of an entry, which an aterm that reads no seen file reads, never trails
/// a time noted beside it by more than [`SEEN_BEHIND_MS`]: the time that would is a whole write
/// carrying every time noted, of the book and of the copy of the issuer's book alike.
/// NEGATIVE CONTROL: drop `!seen::trails` from `HeldBook::update_at`: the book's inode stays.
/// Or from `HeldBook::keep_issuer_book_at`: the copy's does. Or have `seen::trails` compare
/// with `>=`: the time exactly the bound later writes the book.
#[test]
fn a_books_own_time_never_trails_a_noted_one_by_more_than_the_bound() {
    let state = State::new("seen-behind");
    let dir = state.dir();
    update_peers(dir, |book| {
        record(book, joined(0xa1, "a.local", [10, 0, 0, 1], 7100))?;
        record(book, joined(0xb2, "b.local", [10, 0, 0, 2], 7100)).map(|_| ())
    })
    .unwrap();
    let written = on_disk(dir, PEERS_FILE);
    hear(dir, 0xb2, T0 + SEEN_BEHIND_MS, seen::Pace::Now);
    hear(dir, 0xa1, T0 + 1_000, seen::Pace::Now);
    assert_eq!(on_disk(dir, PEERS_FILE), written, "inside the bound");
    assert!(seen_text(dir).is_some());

    hear(dir, 0xb2, T0 + SEEN_BEHIND_MS + 1, seen::Pace::Now);
    assert_ne!(on_disk(dir, PEERS_FILE).0, written.0, "past it");
    assert_eq!(seen_text(dir), None);
    let (book, _) = read_book_as_written(dir, None).unwrap();
    assert_eq!(
        book.iter()
            .map(|peer| peer.seen_ms.unwrap() - T0)
            .collect::<Vec<_>>(),
        [1_000, SEEN_BEHIND_MS + 1],
        "the write carries the other member's noted time too"
    );

    let issuer = node_of(0x11);
    let a = copy(joined(0xa1, "a.local", [10, 0, 0, 1], 7100));
    let heard = |by: u64| Peer {
        seen_ms: Some(T0 + by),
        ..a.clone()
    };
    keep_copy(dir, &issuer, std::slice::from_ref(&a)).unwrap();
    let written = on_disk(dir, ISSUER_BOOK_FILE);
    keep_copy(dir, &issuer, &[heard(SEEN_BEHIND_MS)]).unwrap();
    assert_eq!(on_disk(dir, ISSUER_BOOK_FILE), written);
    assert!(seen_text(dir).is_some());
    keep_copy(dir, &issuer, &[heard(SEEN_BEHIND_MS + 1)]).unwrap();
    assert_ne!(on_disk(dir, ISSUER_BOOK_FILE).0, written.0);
    assert_eq!(seen_text(dir), None);
    assert_eq!(read_issuer_book(dir, &issuer), [heard(SEEN_BEHIND_MS + 1)]);
}

/// A reader holds no lock, so a whole write, which carries the noted times into the book and
/// then drops them, can come between its two reads: the times are taken before the book, so
/// the reader pairs them with the book of before the write or finds them in the book of after
/// it, and never reads the old book with the times already gone.
/// NEGATIVE CONTROL: have `seen::before` run `read` and then take the times: the member reads
/// as last heard at the old book's own time.
#[test]
fn a_whole_write_between_a_readers_two_reads_loses_no_time() {
    let state = State::new("seen-torn");
    let dir = state.dir();
    update_peers(dir, |book| {
        record(book, joined(0xa1, "a.local", [10, 0, 0, 1], 7100))?;
        record(book, joined(0xb2, "b.local", [10, 0, 0, 2], 7100)).map(|_| ())
    })
    .unwrap();
    let issuer = node_of(0x11);
    let b = copy(joined(0xb2, "b.local", [10, 0, 0, 2], 7100));
    keep_copy(dir, &issuer, std::slice::from_ref(&b)).unwrap();
    hear(dir, 0xb2, T0 + 180_000, seen::Pace::Now);
    keep_copy(
        dir,
        &issuer,
        &[Peer {
            seen_ms: Some(T0 + 170_000),
            ..b.clone()
        }],
    )
    .unwrap();
    let text = seen_text(dir).unwrap();
    assert!(
        text.contains("\nseen node=") && text.contains("\ncopy node="),
        "{text}"
    );

    // The writer's whole write of each book lands just after the reader read that book's file.
    let (times, (mut book, mut copies)) = seen::before(dir, || {
        let book = read_book_as_written(dir, None).unwrap().0;
        let copies = issuer_copy(dir).unwrap().copies.unwrap();
        update_peers(dir, |book| {
            record(book, joined(0xc3, "c.local", [10, 0, 0, 3], 7100)).map(|_| ())
        })
        .unwrap();
        keep_copy(
            dir,
            &issuer,
            &[Peer {
                version: Some("0.199.0".to_owned()),
                seen_ms: Some(T0 + 170_000),
                ..b.clone()
            }],
        )
        .unwrap();
        (book, copies)
    });
    assert_eq!(seen_text(dir), None, "the writes dropped the lines");
    assert_eq!(book.len(), 2, "the reader has the book of before the write");
    times.raise(seen::Of::Book, &mut book);
    times.raise(seen::Of::Copy, &mut copies);
    assert_eq!(book[1].seen_ms, Some(T0 + 180_000));
    assert_eq!(copies[0].seen_ms, Some(T0 + 170_000));

    // And one that lands before the reader reads the file is in the file it reads.
    hear(dir, 0xa1, T0 + 190_000, seen::Pace::Now);
    let (times, mut book) = seen::before(dir, || {
        update_peers(dir, |book| {
            record(book, joined(0xd4, "d.local", [10, 0, 0, 4], 7100)).map(|_| ())
        })
        .unwrap();
        read_book_as_written(dir, None).unwrap().0
    });
    times.raise(seen::Of::Book, &mut book);
    assert_eq!(book.len(), 4);
    assert_eq!(book[0].seen_ms, Some(T0 + 190_000));
    assert_eq!(seen_of(dir, 0xa1), Some(T0 + 190_000));
}

/// A write of noted times that fails keeps them noted and is tried again a pace later with
/// nothing more noted, not at once and not only when another contact comes.
/// NEGATIVE CONTROL: drop `retry` from `seen::flush_late`: no thread waits after the failure.
/// Or from `seen::finish`: none waits after the failed write at a hold's end. Or have
/// `seen::retry` leave `tried` as it was: the try one second later is made, and fails.
#[test]
fn a_failed_write_of_noted_times_is_tried_again_a_pace_later() {
    let state = State::new("seen-retry");
    let dir = state.dir();
    update_peers(dir, |book| {
        record(book, joined(0xa1, "a.local", [10, 0, 0, 1], 7100)).map(|_| ())
    })
    .unwrap();
    // A folder with a file in it where the seen file goes: no write can replace it.
    let block = |on: bool| {
        let at = dir.join(SEEN_FILE);
        if on {
            let _ = std::fs::remove_file(&at);
            std::fs::create_dir(&at).unwrap();
            std::fs::write(at.join("held"), b"").unwrap();
        } else {
            std::fs::remove_dir_all(&at).unwrap();
        }
    };
    let began = Instant::now();
    hear(dir, 0xa1, T0 + 1_000, seen::Pace::Within(began));
    assert!(!seen::waits(dir), "written at once");
    hear(
        dir,
        0xa1,
        T0 + 2_000,
        seen::Pace::Within(began + Duration::from_secs(10)),
    );
    assert!(seen::waits(dir));

    block(true);
    let failed = began + SEEN_FLUSH_EVERY;
    assert!(seen::flush_late(dir, failed).is_err());
    assert!(seen::waits(dir), "a thread waits to try again");
    assert_eq!(seen_of(dir, 0xa1), Some(T0 + 2_000), "the time stays noted");
    assert!(
        !seen::flush_late(dir, failed + Duration::from_secs(1)).unwrap(),
        "not before a pace has passed since the try"
    );
    assert!(seen::waits(dir));
    block(false);
    assert!(seen::flush_late(dir, failed + SEEN_FLUSH_EVERY).unwrap());
    assert!(!seen::waits(dir));
    let line = |by: u64| format!("seen node={} ms={}\n", node_of(0xa1), T0 + by);
    let text = seen_text(dir).unwrap();
    assert!(text.contains(&line(2_000)), "{text}");

    // The write a noted time that is due makes as its hold ends, failing, is tried again too:
    // the time is noted, so the edit itself is no failure.
    block(true);
    let again = failed + 3 * SEEN_FLUSH_EVERY;
    hear(dir, 0xa1, T0 + 3_000, seen::Pace::Within(again));
    assert_eq!(seen_of(dir, 0xa1), Some(T0 + 3_000), "the time stays noted");
    assert!(seen::waits(dir));
    block(false);
    assert!(seen::flush_late(dir, again + SEEN_FLUSH_EVERY).unwrap());
    let text = seen_text(dir).unwrap();
    assert!(text.contains(&line(3_000)), "{text}");
}

/// ONE HOLD OF THE BOOK'S LOCK CHANGES THE SEEN FILE ONCE, as it ends: the round in which a
/// node job writes both its book and its copy of its issuer's book whole leaves the seen file
/// as it was after each of them and removes it once, where each whole write changed it in
/// turn (a rewrite without the book's lines, then an unlink). Until then this process's reads
/// pass over the lines of a book it wrote whole, so a time that write set earlier stands. A
/// time due in the file and a book written whole in the same hold are one change too.
/// NEGATIVE CONTROL: have `seen::settle` change the file itself (`rewrite(dir, None)`): the
/// file moves inside the hold. Or have `seen::times` take the lines of a settled book: the
/// earlier time reads as the later one inside the hold. Or have `seen::note` write a due time
/// itself (`flush`): the file moves inside the last hold.
#[test]
fn one_hold_of_the_books_lock_changes_the_seen_file_once() {
    let state = State::new("seen-once");
    let dir = state.dir();
    update_peers(dir, |book| {
        record(book, joined(0xa1, "a.local", [10, 0, 0, 1], 7100))?;
        record(book, joined(0xb2, "b.local", [10, 0, 0, 2], 7100)).map(|_| ())
    })
    .unwrap();
    let issuer = node_of(0x11);
    let a = copy(joined(0xa1, "a.local", [10, 0, 0, 1], 7100));
    let heard = |by: u64| Peer {
        seen_ms: Some(T0 + by),
        ..a.clone()
    };
    let set = |node: u8, at: u64| {
        move |book: &mut Vec<Peer>| {
            let peer = book
                .iter_mut()
                .find(|peer| peer.node == Some(node_of(node)))
                .ok_or("no such member")?;
            peer.seen_ms = Some(at);
            Ok::<(), String>(())
        }
    };
    let line =
        |word: &str, node: u8, by: u64| format!("{word} node={} ms={}\n", node_of(node), T0 + by);
    keep_copy(dir, &issuer, std::slice::from_ref(&a)).unwrap();
    hear(dir, 0xb2, T0 + 1_000, seen::Pace::Now);
    keep_copy(dir, &issuer, &[heard(1_000)]).unwrap();
    let beside = on_disk(dir, SEEN_FILE);
    let text = seen_text(dir).unwrap();
    assert!(text.contains(&line("seen", 0xb2, 1_000)), "{text}");
    assert!(text.contains(&line("copy", 0xa1, 1_000)), "{text}");

    // The round that writes both books whole: each time is past the bound.
    let now = Instant::now();
    let past = SEEN_BEHIND_MS + 2_000;
    let (book_was, copy_was) = (on_disk(dir, PEERS_FILE), on_disk(dir, ISSUER_BOOK_FILE));
    {
        let book = hold_book(dir).unwrap();
        book.update_at(set(0xb2, T0 + past), seen::Pace::Within(now))
            .unwrap();
        assert_ne!(on_disk(dir, PEERS_FILE).0, book_was.0, "the book is whole");
        assert_eq!(on_disk(dir, SEEN_FILE), beside, "unchanged at the book");
        book.keep_issuer_book_at(Some((&issuer, &[heard(past)])), seen::Pace::Within(now))
            .unwrap();
        assert_ne!(on_disk(dir, ISSUER_BOOK_FILE).0, copy_was.0);
        assert_eq!(on_disk(dir, SEEN_FILE), beside, "and at the copy");
    }
    assert_eq!(seen_text(dir), None, "one change as the hold ends");
    assert_eq!(seen_of(dir, 0xb2), Some(T0 + past));
    assert_eq!(read_issuer_book(dir, &issuer), [heard(past)]);

    // A whole write that sets a time earlier than its line: the line is still in the file
    // inside the hold, and no read of this process takes it.
    hear(dir, 0xa1, T0 + 5_000, seen::Pace::Now);
    let beside = on_disk(dir, SEEN_FILE);
    {
        let book = hold_book(dir).unwrap();
        book.update_at(set(0xa1, T0 + 100), seen::Pace::Now)
            .unwrap();
        assert_eq!(on_disk(dir, SEEN_FILE), beside);
        assert_eq!(seen_of(dir, 0xa1), Some(T0 + 100));
    }
    assert_eq!(seen_text(dir), None);
    assert_eq!(seen_of(dir, 0xa1), Some(T0 + 100));

    // A time due in the file, then the copy written whole, in one hold: one change, which
    // holds the time and none of the copy's lines.
    keep_copy(dir, &issuer, &[heard(past + 1_000)]).unwrap();
    let beside = on_disk(dir, SEEN_FILE);
    assert!(
        seen_text(dir)
            .unwrap()
            .contains(&line("copy", 0xa1, past + 1_000))
    );
    {
        let book = hold_book(dir).unwrap();
        book.update_at(set(0xa1, T0 + 7_000), seen::Pace::Now)
            .unwrap();
        assert_eq!(
            on_disk(dir, SEEN_FILE),
            beside,
            "the due time waits for the end"
        );
        assert_eq!(seen_of(dir, 0xa1), Some(T0 + 7_000), "and is read at once");
        book.keep_issuer_book_at(Some((&issuer, &[heard(3 * past)])), seen::Pace::Now)
            .unwrap();
        assert_eq!(on_disk(dir, SEEN_FILE), beside);
    }
    assert_eq!(
        seen_text(dir).unwrap().lines().skip(1).collect::<Vec<_>>(),
        [line("seen", 0xa1, 7_000).trim_end()]
    );
}

/// A process that stays writes what it holds unwritten as it ends, whatever its pace.
/// NEGATIVE CONTROL: have `seen::flush_now` return before its write: the time is not on disk.
#[test]
fn a_process_that_ends_in_order_writes_the_times_it_holds() {
    let state = State::new("seen-end");
    let dir = state.dir();
    update_peers(dir, |book| {
        record(book, joined(0xa1, "a.local", [10, 0, 0, 1], 7100)).map(|_| ())
    })
    .unwrap();
    write_seen_noted(dir, Duration::ZERO).unwrap();
    assert_eq!(seen_text(dir), None, "nothing is noted");
    let began = Instant::now();
    hear(dir, 0xa1, T0 + 1_000, seen::Pace::Within(began));
    hear(dir, 0xa1, T0 + 2_000, seen::Pace::Within(began));
    let line = |by: u64| format!("seen node={} ms={}\n", node_of(0xa1), T0 + by);
    assert!(seen_text(dir).unwrap().contains(&line(1_000)));
    write_seen_noted(dir, Duration::ZERO).unwrap();
    let text = seen_text(dir).unwrap();
    assert!(text.contains(&line(2_000)), "{text}");
}

/// One state directory's pace is its own: a time noted of another directory at an instant far
/// later, as a test that holds its own clock gives one, leaves this directory's last write
/// standing as what its next time is paced by.
/// NEGATIVE CONTROL (2026-11-12): have `seen::note` drop every other directory whose last write
/// is [`SEEN_FLUSH_EVERY`] before the instant it is given: the second time is on disk at once
/// (three tests of this file failed that way when another's later instant fell between their
/// two notes).
#[test]
fn another_directorys_later_instant_leaves_this_ones_pace_standing() {
    let (state, other) = (State::new("seen-own-pace"), State::new("seen-other-pace"));
    for dir in [state.dir(), other.dir()] {
        update_peers(dir, |book| {
            record(book, joined(0xa1, "a.local", [10, 0, 0, 1], 7100)).map(|_| ())
        })
        .unwrap();
    }
    let dir = state.dir();
    let began = Instant::now();
    hear(dir, 0xa1, T0 + 1_000, seen::Pace::Within(began));
    let beside = on_disk(dir, SEEN_FILE);
    hear(
        other.dir(),
        0xa1,
        T0 + 1_000,
        seen::Pace::Within(began + 2 * SEEN_FLUSH_EVERY),
    );
    hear(dir, 0xa1, T0 + 2_000, seen::Pace::Within(began));
    assert_eq!(
        on_disk(dir, SEEN_FILE),
        beside,
        "the second time waits for this directory's own pace"
    );
    assert_eq!(seen_of(dir, 0xa1), Some(T0 + 2_000));
}

/// The copy of the issuer's book is written when the issuer's book changed: a read that brings
/// the copies the file holds writes nothing, and one that brings only later times writes them
/// beside it.
/// NEGATIVE CONTROL: have `HeldBook::keep_issuer_book_at` skip its compare with the kept
/// copies: the copy's inode moves at the second keep.
#[test]
fn the_copy_of_the_issuers_book_is_written_when_the_issuers_book_changed() {
    let state = State::new("seen-copy");
    let dir = state.dir();
    let issuer = node_of(0x11);
    let a = copy(joined(0xa1, "a.local", [10, 0, 0, 1], 7100));
    let b = copy(joined(0xb2, "b.local", [10, 0, 0, 2], 7100));
    keep_copy(dir, &issuer, &[a.clone(), b.clone()]).unwrap();
    let written = on_disk(dir, ISSUER_BOOK_FILE);

    keep_copy(dir, &issuer, &[a.clone(), b.clone()]).unwrap();
    assert_eq!(on_disk(dir, ISSUER_BOOK_FILE), written);
    assert_eq!(seen_text(dir), None);

    let heard = Peer {
        seen_ms: Some(T0 + 180_000),
        ..b.clone()
    };
    keep_copy(dir, &issuer, &[a.clone(), heard.clone()]).unwrap();
    assert_eq!(on_disk(dir, ISSUER_BOOK_FILE), written);
    let text = seen_text(dir).expect("the copy's time is on disk");
    assert!(
        text.contains(&format!(
            "copy node={} ms={}\n",
            node_of(0xb2),
            T0 + 180_000
        )),
        "{text}"
    );
    assert_eq!(read_issuer_book(dir, &issuer), [a.clone(), heard.clone()]);

    // A time the issuer's book holds earlier (it lost what it had not written) is its book's.
    keep_copy(dir, &issuer, &[a.clone(), b.clone()]).unwrap();
    assert_ne!(on_disk(dir, ISSUER_BOOK_FILE).0, written.0);
    assert_eq!(read_issuer_book(dir, &issuer), [a.clone(), b.clone()]);
    assert_eq!(seen_text(dir), None);

    // Another field, or another issuer's book, is a whole write.
    let rewritten = on_disk(dir, ISSUER_BOOK_FILE);
    let upgraded = Peer {
        version: Some("0.199.0".to_owned()),
        ..heard
    };
    keep_copy(dir, &issuer, &[a.clone(), upgraded.clone()]).unwrap();
    assert_ne!(on_disk(dir, ISSUER_BOOK_FILE).0, rewritten.0);
    assert_eq!(
        read_issuer_book(dir, &issuer),
        [a.clone(), upgraded.clone()]
    );
    let other = node_of(0x12);
    keep_copy(dir, &other, &[a.clone(), upgraded.clone()]).unwrap();
    assert!(read_issuer_book(dir, &issuer).is_empty());
    assert_eq!(read_issuer_book(dir, &other), [a, upgraded]);
}

/// A leave's removal of the book takes the seen file with it, and a book's removal its lines.
/// NEGATIVE CONTROL: drop `seen::forget` from `Leaving::remove`: the file outlives the leave.
#[test]
fn the_seen_file_goes_with_the_books() {
    let state = State::new("seen-leave");
    let dir = state.dir();
    let issuer = node_of(0x11);
    let a = joined(0xa1, "a.local", [10, 0, 0, 1], 7100);
    update_peers(dir, |book| record(book, a.clone()).map(|_| ())).unwrap();
    keep_copy(dir, &issuer, &[copy(a.clone())]).unwrap();
    hear(dir, 0xa1, T0 + 1_000, seen::Pace::Now);
    keep_copy(
        dir,
        &issuer,
        &[copy(Peer {
            seen_ms: Some(T0 + 2_000),
            ..a.clone()
        })],
    )
    .unwrap();
    let text = seen_text(dir).unwrap();
    assert!(
        text.contains("\nseen node=") && text.contains("\ncopy node="),
        "{text}"
    );

    assert!(hold_book(dir).unwrap().remove(ISSUER_BOOK_FILE).unwrap());
    let text = seen_text(dir).unwrap();
    assert!(
        text.contains("\nseen node=") && !text.contains("\ncopy node="),
        "{text}"
    );

    leave_held(dir, |leaving| leaving.remove(PEERS_FILE).unwrap()).unwrap();
    assert_eq!(seen_text(dir), None);
}

fn facts_payload(at_ms: u64, sessions: u32) -> String {
    format!(
        "OK 2\ninstance pid=41 host=m7 version=0.198.0 sessions={sessions} level=facts cut=0 \
         at_ms={at_ms} node={} instances=1 access=read-only\naccount name=a at_ms=5\n",
        node_of(0x33)
    )
}

/// A push that says the facts its record holds, rendered later, keeps the record's bytes and
/// inode and moves only its modification time, the receipt time every reader takes; a push that
/// says anything else, under another key included, replaces the record.
/// NEGATIVE CONTROL: have `publish_pushed_facts` always `write_private`: the inode moves at the
/// second push. Or drop its `set_modified`: the record still reads as received in 1970.
#[test]
fn a_push_of_the_facts_its_record_holds_moves_only_the_records_time() {
    let state = State::new("facts-same");
    let node = node_of(0x33);
    let key = NodeIdentity::from_seed(&node, &[0x11; 32]).unwrap().key();
    let other = NodeIdentity::from_seed(&node, &[0x22; 32]).unwrap().key();
    let dir = state.dir().join(SWARM_FACTS_DIR);
    let file = format!("{node}.facts");
    let received = || {
        read_admitted_pushed_facts(state.dir(), &[], &[], None)
            .into_iter()
            .find(|record| record.node == node)
            .expect("the record is admitted")
            .received_at
            .expect("it has a receipt time")
    };
    let long_ago = || {
        std::fs::File::options()
            .write(true)
            .open(dir.join(&file))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
            .unwrap();
    };

    store_pushed_facts_with_key(state.dir(), &node, Some(key), &facts_payload(1_000, 3)).unwrap();
    let written = on_disk(&dir, &file);
    long_ago();
    assert_eq!(received(), std::time::UNIX_EPOCH);

    store_pushed_facts_with_key(state.dir(), &node, Some(key), &facts_payload(181_000, 3)).unwrap();
    assert_eq!(on_disk(&dir, &file), written, "the record is not replaced");
    assert_ne!(
        received(),
        std::time::UNIX_EPOCH,
        "its receipt time moved from the one planted"
    );
    assert_eq!(
        read_pushed_facts_of(state.dir(), &node).as_deref(),
        Some(facts_payload(1_000, 3).as_str())
    );

    // A fact that moved is a new record; only the header's own time is passed over.
    store_pushed_facts_with_key(state.dir(), &node, Some(key), &facts_payload(361_000, 4)).unwrap();
    let moved = on_disk(&dir, &file);
    assert_ne!(moved.0, written.0);
    assert_eq!(
        read_pushed_facts_of(state.dir(), &node).as_deref(),
        Some(facts_payload(361_000, 4).as_str())
    );
    let later_line = facts_payload(361_000, 4).replace("at_ms=5", "at_ms=6");
    store_pushed_facts_with_key(state.dir(), &node, Some(key), &later_line).unwrap();
    let relined = on_disk(&dir, &file);
    assert_ne!(relined.0, moved.0, "a later line's time is a fact");
    // The same facts under another proved key are another record: its key is in its head.
    store_pushed_facts_with_key(state.dir(), &node, Some(other), &later_line).unwrap();
    assert_ne!(on_disk(&dir, &file).0, relined.0);
}

/// The time a payload was rendered at is cut from its `instance` header alone.
#[test]
fn only_the_instance_headers_render_time_is_set_aside() {
    let record = |payload: &str| format!("{PUSHED_FACTS_MAGIC}-\n{payload}").into_bytes();
    let a = record(&facts_payload(1, 3));
    let b = record(&facts_payload(22_222, 3));
    assert_eq!(around_rendered_at(&a), around_rendered_at(&b));
    let (head, tail) = around_rendered_at(&a);
    assert!(head.ends_with(b"cut=0") && tail.starts_with(b" node="));
    for odd in [
        "",
        "OK 1\n",
        "OK 1\ninstance pid=1\n",
        "OK 1\naccount at_ms=5\n",
    ] {
        let odd = record(odd);
        assert_eq!(around_rendered_at(&odd), (&odd[..], &[][..]));
    }
    assert_ne!(
        around_rendered_at(&record(&facts_payload(1, 3))),
        around_rendered_at(&record(&facts_payload(1, 4)))
    );
}
