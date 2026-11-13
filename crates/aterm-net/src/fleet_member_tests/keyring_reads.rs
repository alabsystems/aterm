// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! What a read of the keys this machine binds reads of its state directory: the node's watch
//! takes one at each tick a drive is open across.

use super::*;
use aterm_uds::private_file::{settle, tally};

/// A member's state directory: its book names the issuer `0xa1` and binds the member `0xd4`,
/// its copy of the issuer's book binds `0xc3`, and a time is noted beside them.
fn member_state(tag: &str) -> (State, Vec<Peer>) {
    let state = State::new(tag);
    write_key(state.dir(), &[0x5a; SECRET_LEN]).unwrap();
    let key = |node: u8| identity(&node_of(node), node).key();
    let book = vec![
        Peer {
            via: Via::Issuer,
            ..keyed(0xa1, key(0xa1), [10, 0, 0, 1], 7100)
        },
        keyed(0xd4, key(0xd4), [10, 0, 0, 4], 7100),
    ];
    write_book(state.dir(), &book);
    keep_copy(state.dir(), &node_of(0xa1), &[copy_of(0xc3)]).unwrap();
    write_private(
        state.dir(),
        SEEN_FILE,
        format!("book node={} 1760000000000\n", node_of(0xd4)).as_bytes(),
    )
    .unwrap();
    (state, book)
}

fn copy_of(node: u8) -> Peer {
    Peer {
        via: Via::IssuerBook,
        ..keyed(
            node,
            identity(&node_of(node), node).key(),
            [10, 0, 0, node],
            7100,
        )
    }
}

/// The names of the files a read listed, and the bytes it took of each.
fn listed(read: &[tally::Read]) -> Vec<(&str, usize)> {
    read.iter()
        .map(|seen| (seen.name.as_str(), seen.bytes))
        .collect()
}

fn len_of(dir: &Path, name: &str) -> usize {
    usize::try_from(std::fs::metadata(dir.join(name)).unwrap().len()).unwrap()
}

/// A read of the keys over books nothing wrote since the read before takes the fleet key and
/// not a byte of the book or of the copy of the issuer's book, and opens three files: those
/// two and the key, not the times noted beside the books, which no key is part of. The books
/// are still judged as they stand at that read:
/// - a book written again (a rename, as every write of it is) is read whole at the next read,
///   and the member it no longer binds is bound to no key;
/// - so is the copy;
/// - a state directory open to others binds nothing, though no file in it changed.
///
/// The parses kept are this thread's own (`kept_parse!`), so no other test's state directories
/// push these out of the few kept between two reads here.
///
/// NEGATIVE CONTROL: `KeptParse::read` reading its file at every call lists both books at the
/// second read; a pin found by its path alone (`self.stamp == opened` dropped from
/// `Pin::stands`) reads no book after the one that binds `0xd4` is replaced; the keys read
/// through `book_and_copies` judge the directory five times, twice for `fleet-seen`.
#[test]
fn a_keyring_read_of_books_nothing_wrote_reads_no_book_bytes() {
    let (state, book) = member_state("keyring-reads");
    let dir = state.dir();
    let key = |node: u8| Some(identity(&node_of(node), node).key());
    settle::none(|| {
        let ring = Keyring::read(dir);
        for node in [0xa1, 0xd4, 0xc3] {
            assert_eq!(ring.bound(&node_of(node)), key(node), "{node:#x}");
        }
        let ((again, read), seen) = tally::directories(|| tally::counted(|| Keyring::read(dir)));
        assert_eq!(again, ring);
        assert_eq!(
            listed(&read),
            [(KEY_FILE, len_of(dir, KEY_FILE))],
            "books nothing wrote are not read"
        );
        assert_eq!(
            seen.judged,
            [(dir.to_path_buf(), 3)],
            "the key, the book and the copy are each opened and judged, and nothing else"
        );

        write_book(dir, &book[..1]);
        let (ring, read) = tally::counted(|| Keyring::read(dir));
        assert_eq!(
            listed(&read),
            [
                (KEY_FILE, len_of(dir, KEY_FILE)),
                (PEERS_FILE, len_of(dir, PEERS_FILE))
            ],
            "a book written again is read at the next read, whole"
        );
        assert_eq!(ring.bound(&node_of(0xd4)), None);
        assert_eq!(ring.bound(&node_of(0xc3)), key(0xc3));

        keep_copy(dir, &node_of(0xa1), &[copy_of(0xe5)]).unwrap();
        let (ring, read) = tally::counted(|| Keyring::read(dir));
        assert_eq!(
            listed(&read),
            [
                (KEY_FILE, len_of(dir, KEY_FILE)),
                (ISSUER_BOOK_FILE, len_of(dir, ISSUER_BOOK_FILE))
            ],
            "and so is the copy"
        );
        assert_eq!(ring.bound(&node_of(0xc3)), None);
        assert_eq!(ring.bound(&node_of(0xe5)), key(0xe5));

        mode(dir, 0o755);
        assert_eq!(
            Keyring::read(dir),
            Keyring::default(),
            "a directory open to others binds nothing"
        );
        mode(dir, 0o700);
        assert_eq!(Keyring::read(dir), ring);
    });
}
