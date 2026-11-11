// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[derive(Debug)]
struct Scratch {
    _dir: aterm_tempfile::TempDir,
    path: PathBuf,
}

impl std::ops::Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<std::ffi::OsStr> for Scratch {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.path.as_os_str()
    }
}

fn scratch(tag: &str) -> Scratch {
    let dir = crate::test_scratch_dir(tag);
    let path = dir.path().join("s");
    Scratch { _dir: dir, path }
}

fn base(inc: u64) -> u64 {
    astream_broker::incarnation_base(inc).expect("in the layout")
}

fn will(inc: u64) -> u64 {
    astream_broker::will_seq(inc).expect("in the layout")
}

#[test]
fn every_reserved_sequence_sits_in_astreams_incarnation_layout() {
    let dir = scratch("layout");
    let st = StateDir::open(&dir).expect("open");
    for inc in [0, 1, 7, INC_MAX] {
        let block = st.reserve_sequence_block(inc, 3).expect("a block");
        let base = base(inc);
        let will = will(inc);
        assert!(
            base < *block.start() && *block.end() < will,
            "inc {inc}: {block:?} strictly inside ({base}, {will})"
        );
        assert_eq!(astream_broker::incarnation_of(*block.end()), Some(inc));
        assert_eq!(
            st.incarnation(),
            inc,
            "the dir answers for the block's incarnation"
        );
    }
    assert!(
        st.reserve_sequence_block(INC_MAX + 1, 1).is_err(),
        "past INC_MAX the layout has no space"
    );
    let corrupt = scratch("layout-ack-half");
    let st = StateDir::open(&corrupt).expect("open");
    std::fs::write(
        corrupt.join("seq"),
        (astream_broker::ACK_SEQ_BASE | 5).to_string(),
    )
    .expect("seed a seq in the ack half");
    assert!(
        st.take_incarnation(0).is_err(),
        "a seq no incarnation owns refuses the take; it is never incarnation 0"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&corrupt);
}

/// Each node's handed-over inbox commit is its own line: the hand-over of a bridge of the node
/// a key rotation gave this Mac keeps the one a bridge of the node before left, which the node
/// service's own-inbox move makes (§11 C3b); a raise keeps the higher; making one leaves the
/// other, and the file goes with the last. A bare offset, as a build before C3b wrote one,
/// is no node's.
#[test]
fn each_nodes_handed_over_inbox_commit_is_its_own() {
    let dir = scratch("owed-per-node");
    let st = StateDir::open(&dir).expect("open");
    st.set_inbox_owed("n-a", 5).expect("the old node's");
    st.set_inbox_owed("n-b", 9).expect("the new node's");
    assert_eq!(
        (st.inbox_owed("n-a"), st.inbox_owed("n-b")),
        (Some(5), Some(9)),
        "both kept"
    );
    st.set_inbox_owed("n-a", 3).expect("a lower one");
    assert_eq!(st.inbox_owed("n-a"), Some(5), "the higher stays");
    st.set_inbox_owed("n-a", 6).expect("a higher one");
    assert_eq!(st.inbox_owed("n-a"), Some(6), "raised");
    st.clear_inbox_owed("n-b").expect("made");
    assert_eq!(
        (st.inbox_owed("n-a"), st.inbox_owed("n-b")),
        (Some(6), None),
        "the other stays"
    );
    st.clear_inbox_owed("n-a").expect("made");
    assert!(!dir.join("inbox-owed").exists(), "none left, no file");
    std::fs::write(dir.join("inbox-owed"), "7\n").expect("a bare offset");
    assert_eq!(
        StateDir::open(&dir).expect("reopen").inbox_owed("n-a"),
        None
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE OWN-INBOX MOVE'S GATE READS NOTHING IT COULD NOT READ AS ABSENT (§11 C3b): `node` and
/// `inbox-owed` read through [`StateDir::node_present`] and [`StateDir::inbox_owed_present`]
/// are `None` only where the file is not there, and an `Err` where it exists and cannot be
/// read, which keeps the move shut; the lenient readers take the same file for none (the
/// control).
#[test]
fn the_gates_readers_take_an_unreadable_file_for_no_answer() {
    let dir = scratch("gate-reads");
    let st = StateDir::open(&dir).expect("open");
    assert_eq!(st.node_present().expect("absent"), None);
    assert_eq!(st.inbox_owed_present("n-a").expect("absent"), None);
    st.name_node("n-a").expect("a node");
    st.set_inbox_owed("n-a", 5).expect("a hand-over");
    assert_eq!(st.node_present().expect("read").as_deref(), Some("n-a"));
    assert_eq!(st.inbox_owed_present("n-a").expect("read"), Some(5));
    assert_eq!(st.inbox_owed_present("n-b").expect("read"), None);
    for name in ["node", "inbox-owed"] {
        std::fs::remove_file(dir.join(name)).expect("the file");
        std::fs::create_dir(dir.join(name)).expect("a file that cannot be read");
    }
    assert_eq!(
        (st.node(), st.inbox_owed("n-a")),
        (None, None),
        "the lenient readers take it for none"
    );
    assert!(
        st.node_present().is_err(),
        "an unreadable node is no answer"
    );
    assert!(
        st.inbox_owed_present("n-a").is_err(),
        "an unreadable hand-over is no answer"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_sequence_block_is_on_disk_before_use_and_never_reaches_the_wills_number() {
    let dir = scratch("seqblock");
    let st = StateDir::open(&dir).expect("open");
    let block = st.reserve_sequence_block(3, 64).expect("a block");
    assert_eq!(block, base(3) + 1..=base(3) + 64);
    assert_eq!(
        st.sequence(),
        *block.end(),
        "the END is what the file holds"
    );
    let reopened = StateDir::open(&dir).expect("reopen");
    assert_eq!(
        reopened.reserve_next_sequence(3).expect("one more"),
        block.end() + 1,
        "a reopen starts above the whole block, however little of it was used"
    );
    assert_eq!(
        reopened.reserve_sequence_block(4, 2).expect("inc 4"),
        base(4) + 1..=base(4) + 2
    );
    std::fs::write(dir.join("seq"), (will(5) - 3).to_string()).expect("seed near the top");
    let short = reopened
        .reserve_sequence_block(5, 64)
        .expect("a short block");
    assert_eq!(short, will(5) - 2..=will(5) - 1);
    assert!(
        reopened.reserve_sequence_block(5, 64).is_err(),
        "nothing below the will's number is left"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_directory_flush_tolerates_a_gone_directory_and_refuses_a_broken_path() {
    use std::os::unix::fs::PermissionsExt as _;
    struct RestoreMode(PathBuf, std::fs::Permissions);
    impl Drop for RestoreMode {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, self.1.clone());
        }
    }
    let owned = aterm_tempfile::Builder::new()
        .prefix("syncdir")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let dir = owned.path();
    let locked = dir.join("locked");
    std::fs::create_dir_all(&locked).expect("scratch");
    let flush = aterm_durable::Flush::Ordered;
    assert!(sync_dir(dir).is_ok());
    assert!(sync_dir_required_as(dir, flush).is_ok());
    assert!(sync_dir(&dir.join("gone")).is_ok());
    assert_eq!(
        sync_dir_required_as(&dir.join("gone"), flush).map_err(|e| e.kind()),
        Err(io::ErrorKind::NotFound)
    );
    let known = locked.join("known");
    std::fs::write(&known, b"still reachable").expect("a known file");
    let restore = RestoreMode(
        locked.clone(),
        std::fs::metadata(&locked).expect("its mode").permissions(),
    );
    let mode = |m| std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(m));
    mode(0o300).expect("write but not read");
    let file = std::fs::read(&known);
    let direct = std::fs::File::open(&locked)
        .map(|_| ())
        .map_err(|e| e.kind());
    let required = sync_dir_required_as(&locked, flush).map_err(|e| e.kind());
    let opened = sync_dir(&locked);
    drop(restore);
    assert_eq!(
        file.expect("known file remains readable"),
        b"still reachable"
    );
    assert_eq!(
        required, direct,
        "required flush preserves the directory-open result, even when only known files are reachable"
    );
    assert!(opened.is_ok());
    std::fs::write(dir.join("file"), b"").expect("a file");
    assert_eq!(
        sync_dir(&dir.join("file").join("dir")).map_err(|e| e.kind()),
        Err(io::ErrorKind::NotADirectory)
    );
}

/// A tolerated flush goes on exactly where astream's rule says the entries cannot be
/// flushed: the volume's `ENOTSUP` (Darwin's 45, Linux's 95) and on Apple `ENODEV` (19,
/// what a full flush of `/dev/null` answers) are tolerated, and `EIO` is returned.
/// `ENODEV` keeps the errno half under test: the Trust std reads Darwin's 45 as
/// `Unsupported` (a stock std leaves it uncategorized), but no std names 19 by a kind the
/// rule reads, so a list of kinds alone misses it. CONTROL: 19's kind alone is not
/// tolerated.
#[test]
fn a_tolerated_flush_goes_on_past_enotsup_and_returns_eio() {
    let refused: &[i32] = if cfg!(target_vendor = "apple") {
        let enodev = io::Error::from_raw_os_error(libc::ENODEV);
        assert!(
            tolerate_dir_flush(Err(io::Error::from(enodev.kind()))).is_err(),
            "CONTROL: std names ENODEV by kind ({enodev:?}); the errno half is untested"
        );
        &[libc::ENOTSUP, libc::ENODEV]
    } else {
        &[libc::ENOTSUP]
    };
    for &errno in refused {
        assert!(
            tolerate_dir_flush(Err(io::Error::from_raw_os_error(errno))).is_ok(),
            "errno {errno}"
        );
    }
    assert_eq!(
        tolerate_dir_flush(Err(io::Error::from_raw_os_error(libc::EIO)))
            .map_err(|e| e.raw_os_error()),
        Err(Some(libc::EIO))
    );
}

#[test]
fn the_node_id_is_minted_once_and_survives_reopen() {
    let dir = scratch("node");
    let first = StateDir::open(&dir)
        .expect("open")
        .node_id(|| "n-0123456789abcdef".to_string())
        .expect("mint");
    let second = StateDir::open(&dir)
        .expect("reopen")
        .node_id(|| panic!("must not re-mint"))
        .expect("read back");
    assert_eq!(first, second);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_node_file_that_holds_no_node_id_is_kept_aside_and_an_id_minted() {
    let dir = scratch("node-aside");
    let st = StateDir::open(&dir).expect("open");
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
    std::fs::write(dir.join("node"), "h-x\n").expect("a person's id where the node's goes");
    assert_eq!(
        st.node_id(|| "n-0123456789abcdef".to_string())
            .expect("mint"),
        "n-0123456789abcdef"
    );
    assert_eq!(read("node").as_deref(), Some("n-0123456789abcdef\n"));
    assert_eq!(
        read("node.aside").as_deref(),
        Some("h-x\n"),
        "kept, not overwritten"
    );
    std::fs::write(dir.join("node"), [0xff, b'\n']).expect("bytes that are not text");
    assert_eq!(st.node_id(|| "n-m7".to_string()).expect("mint"), "n-m7");
    assert_eq!(
        std::fs::read(dir.join("node.aside.1")).ok(),
        Some(vec![0xff, b'\n'])
    );
    assert_eq!(
        read("node.aside").as_deref(),
        Some("h-x\n"),
        "the first kept as it was"
    );
    assert_eq!(
        st.node_id(|| panic!("must not re-mint")).expect("kept"),
        "n-m7"
    );
    assert!(read("node.aside.2").is_none(), "a node id is not set aside");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn only_a_node_file_with_bytes_that_the_new_id_replaced_stays_aside() {
    let dir = scratch("node-take-back");
    let st = StateDir::open(&dir).expect("open");
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
    std::fs::write(dir.join("node"), "").expect("an empty node file");
    assert_eq!(st.node_id(|| "n-e".to_string()).expect("mint"), "n-e");
    assert!(
        read("node.aside").is_none(),
        "an empty node file held nothing to keep"
    );

    std::fs::write(dir.join("node"), "h-x\n").expect("a person's id where the node's goes");
    st.refuse_writes_of(Some("node"));
    for _ in 0..2 {
        assert!(
            st.node_id(|| "n-a".to_string()).is_err(),
            "the write is refused"
        );
        assert_eq!(read("node").as_deref(), Some("h-x\n"), "left as it was");
        assert!(
            read("node.aside").is_none(),
            "a failed write takes its link back"
        );
    }
    st.refuse_writes_of(None);
    st.set_inc_seam(IncSeam {
        kill_at: Some(IncStep::Renamed),
        ..Default::default()
    });
    assert!(
        st.node_id(|| "n-b".to_string()).is_err(),
        "the flush after the rename fails"
    );
    st.set_inc_seam(IncSeam::default());
    assert_eq!(read("node").as_deref(), Some("n-b\n"));
    assert_eq!(
        read("node.aside").as_deref(),
        Some("h-x\n"),
        "once the write replaced the file, its link is the only copy and stays"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_topic_cursor_line_parses_or_is_dropped_beside_its_neighbours() {
    let c = |next| TopicCursor { next };
    assert_eq!(
        TopicCursor::parse("build.failed 42"),
        Some(("build.failed".to_string(), c(42)))
    );
    assert_eq!(
        TopicCursor::parse("old.topic 3 7"),
        Some(("old.topic".to_string(), c(3)))
    );
    for bad in [
        "",
        "onlytopic",
        "t x",
        "t 1 y",
        "t 1 2 3",
        "t +5",
        "T 5",
        "a/b 5",
        "a* 5",
        "> 5",
    ] {
        assert_eq!(TopicCursor::parse(bad), None, "{bad:?}");
    }
    let dir = scratch("topics");
    let st = StateDir::open(&dir).expect("open");
    let mut good = BTreeMap::new();
    good.insert("a".to_string(), c(10));
    good.insert("b".to_string(), c(20));
    st.set_topics("s-x", &good).expect("write");
    assert_eq!(st.topics("s-x"), good);
    std::fs::write(
        st.path("topics/s-x"),
        "a 10\nthis is not a cursor \x1b[2J\nb 20\n",
    )
    .expect("corrupt one line");
    assert_eq!(st.topics("s-x"), good, "the neighbours survive");
    st.forget_topics("s-x");
    assert!(st.topics("s-x").is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// EVERY FILE THE STATE DIRECTORY HOLDS IS THE OWNER'S ALONE ([`STATE_FILE_MODE`]): a post's
/// `key=` and a session's topic names are caller text, kept no wider than the 0600 journal
/// keeps the key, whatever the umask lets a plain create make.
#[test]
fn every_file_the_state_directory_holds_is_the_owners_alone() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("modes");
    let st = StateDir::open(&dir).expect("open");
    st.set_key_seq("s-x", "QZP6KEY", 7).expect("a key");
    let mut topics = BTreeMap::new();
    topics.insert("build.done".to_string(), TopicCursor { next: 3 });
    st.set_topics("s-x", &topics).expect("topics");
    let mut floors = BTreeMap::new();
    floors.insert("build.done".to_string(), 3);
    st.set_topic_floors("s-x", &floors).expect("floors");
    st.set_seen_off("s-x", 9).expect("seen");
    st.set_halt_acked(4).expect("halt-acked");
    st.set_inbox_owed("n-a", 5).expect("inbox-owed");
    st.set_self_acked(&std::collections::BTreeSet::from([1, 2]), 0, 8)
        .expect("acked");
    st.append_self_acked(None, &[3]).expect("acked, appended");
    st.raise_logfloor(11).expect("logfloor");
    let mut seen = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("a dir").flatten() {
            let meta = e.metadata().expect("meta");
            if meta.is_dir() {
                stack.push(e.path());
            } else {
                let rel = e.path().strip_prefix(&dir).expect("under").to_path_buf();
                seen.push((rel, meta.permissions().mode() & 0o777));
            }
        }
    }
    seen.sort();
    for want in [
        "keys/s-x",
        "topics/s-x",
        "unhandled/s-x",
        "acked",
        "logfloor",
    ] {
        assert!(
            seen.iter().any(|(rel, _)| rel == Path::new(want)),
            "{want} written: {seen:?}"
        );
    }
    let wide: Vec<_> = seen.iter().filter(|(_, m)| m & 0o077 != 0).collect();
    assert!(wide.is_empty(), "files others may read: {wide:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_broadcast_refill_floor_round_trips_and_goes_with_its_cursors_and_its_log() {
    let dir = scratch("unhandled");
    let st = StateDir::open(&dir).expect("open");
    let mut floors = BTreeMap::new();
    floors.insert("a".to_string(), 10);
    floors.insert("b.c".to_string(), 7);
    st.set_topic_floors("s-x", &floors).expect("write");
    assert_eq!(st.topic_floors("s-x").unwrap(), floors);
    std::fs::write(
        st.path("unhandled/s-x"),
        "a 10\nb.c +7\nB 3\nb.c 7\nd 1 2\n",
    )
    .expect("damage some lines");
    assert!(
        st.topic_floors("s-x").is_err(),
        "damaged state is not an empty floor"
    );
    st.set_topic_floors("s-x", &floors).expect("repair");
    assert_eq!(st.topic_floors("s-x").unwrap(), floors);

    st.set_topic_floors("s-x", &BTreeMap::new())
        .expect("an empty set");
    assert!(
        !st.path("unhandled/s-x").exists(),
        "an empty set is no file"
    );
    assert!(st.set_topic_floors("s-x", &BTreeMap::new()).is_ok());

    st.set_topic_floors("s-x", &floors).expect("write");
    st.forget_topics("s-x");
    assert!(
        st.topic_floors("s-x").unwrap().is_empty(),
        "a departed session's floors go"
    );

    st.set_topic_floors("s-y", &floors).expect("write");
    st.forget_log_offsets(100, None, None, None)
        .expect("re-base");
    assert!(
        st.topic_floors("s-y").unwrap().is_empty(),
        "a floor names a record of the old log"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_seen_watermark_never_goes_backwards() {
    let dir = scratch("seen");
    let st = StateDir::open(&dir).expect("open");
    st.set_seen_off("s-a", 42).expect("write");
    st.set_seen_off("s-a", 7).expect("write");
    assert_eq!(st.seen_off("s-a"), Some(42));
    st.set_seen_off("s-a", 43).expect("write");
    assert_eq!(st.seen_off("s-a"), Some(43));
    assert_eq!(st.seen_off("s-unknown"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_self_acked_set_is_a_set_and_is_bounded() {
    let dir = scratch("acked");
    let st = StateDir::open(&dir).expect("open");
    let mut set = std::collections::BTreeSet::new();
    for off in [10u64, 30, 20] {
        set.insert(off);
        st.set_self_acked(&set, 0, 3).expect("write");
    }
    assert_eq!(st.self_acked(), [10, 20, 30].into_iter().collect());
    assert_eq!(st.halt_acked(), None);
    st.set_halt_acked(91).expect("write");
    st.set_halt_acked(7).expect("write");
    assert_eq!(st.halt_acked(), Some(91));
    st.set_halt_acked(92).expect("write");
    assert_eq!(st.halt_acked(), Some(92));
    set.insert(40);
    st.set_self_acked(&set, 0, 3).expect("write");
    assert_eq!(
        st.self_acked(),
        [20, 30, 40].into_iter().collect(),
        "the oldest is dropped, and 10 is now unknown rather than proven ours"
    );
    assert_eq!(
        st.self_acked_log().floor,
        Some(20),
        "and the floor rises to the oldest kept, so 10 is not accused either"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_pin_is_first_wins_and_reports_the_pinned_node() {
    let dir = scratch("pins");
    let st = StateDir::open(&dir).expect("open");
    assert_eq!(st.pin("s-a", "n-one").expect("pin"), "n-one");
    assert_eq!(
        st.pin("s-a", "n-two").expect("pin"),
        "n-one",
        "a second node claiming a pinned sid is a conflict, not a re-pin"
    );
    assert_eq!(
        StateDir::open(&dir).expect("reopen").pins().get("s-a"),
        Some(&"n-one".to_string())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE JOURNAL'S BOUND (B19): past [`journal::COMPACT_AT`] the next record first rewrites
/// the journal from its index, so a run of retired posts never grows it past the bound and
/// one record, and what is live survives the rewrite and a reopen. CONTROL: below the bound
/// nothing is rewritten.
#[test]
fn the_journal_is_rewritten_from_its_index_past_its_bound_and_keeps_what_is_live() {
    let dir = scratch("journal-bound");
    let st = StateDir::open(&dir).expect("open");
    let pin = |seq| PublishPin {
        seq,
        via_key: false,
        identity: Some(PublishIdentity {
            subject: "/f/f/in/n-a/s-b/n-a/note".into(),
            digest: "d".repeat(64),
        }),
        original: None,
        original_off: None,
        owner: Some(1),
        at: None,
    };
    st.set_post_pin("s-live", 1, &pin(3))
        .expect("a live reservation");
    let entry = InFlight {
        pin: pin(7),
        subject: "/f/f/in/n-a/s-b/n-a/note".into(),
        body: b"v=1 t=1 text=live".to_vec(),
        log: None,
        at: None,
    };
    st.set_in_flight(&entry).expect("a live record in flight");
    let journal = dir.join(journal::FILE);
    let size = || std::fs::metadata(&journal).expect("the journal").len();
    let (mut largest, mut rewrites, mut id) = (0, 0, 0);
    let measured = |write: &dyn Fn()| {
        let before = (size(), st.ops().replaces);
        write();
        (before, (size(), st.ops().replaces))
    };
    while rewrites < 2 {
        id += 1;
        let reserve = || {
            st.set_post_pin("s-a", id, &pin(100 + id))
                .expect("a reservation");
        };
        let retire = || st.clear_post_seq("s-a", id);
        for (before, after) in [measured(&reserve), measured(&retire)] {
            largest = largest.max(after.0);
            if after.1 > before.1 {
                rewrites += 1;
                assert!(after.0 < before.0, "a rewrite holds only what is live");
                assert!(
                    before.0 >= journal::COMPACT_AT,
                    "CONTROL: nothing is rewritten below the bound ({} bytes)",
                    before.0
                );
            }
        }
        assert!(id < 100_000, "the journal was never rewritten");
    }
    assert!(
        largest < journal::COMPACT_AT + 1024,
        "never past the bound and a record: {largest}"
    );
    let again = StateDir::open(&dir).expect("reopen");
    assert_eq!(again.post_pin("s-live", 1).unwrap(), Some(pin(3)));
    assert_eq!(again.in_flight().unwrap(), vec![(7, entry)]);
    assert_eq!(again.post_pin("s-a", id).unwrap(), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A RETIREMENT OR A DROP THE DISK REFUSES (B19 review): the open forgets the post's
/// reservation and the dropped record all the same, as an older build's unlink of their
/// files did on a disk too full to write, so nothing this open does later (an attach's
/// replay, a stop's hand-off) brings them back, and the journal's next record rewrites it
/// without them. CONTROL: a reservation or a record in flight the disk refuses is not held,
/// and a landing it refuses leaves its record in flight, to be cleared again (the bridge
/// delivers a record of its own lane only once its landing is written).
#[test]
fn a_retirement_or_a_drop_the_disk_refuses_is_forgotten_and_never_comes_back() {
    let dir = scratch("refused-removals");
    let st = StateDir::open(&dir).expect("open");
    st.set_post_seq("s-a", 1, 7, false).expect("reserved");
    st.set_in_flight(&in_flight_entry(9)).expect("in flight");
    st.refuse_writes_of(Some(journal::FILE));
    st.clear_post_seq("s-a", 1);
    assert!(
        st.drop_in_flight(9).is_err(),
        "the drop is not on the media, and says so"
    );
    assert_eq!(
        st.post_seq("s-a", 1),
        None,
        "the retired post's reservation"
    );
    assert!(
        st.in_flight().expect("reads").is_empty(),
        "the dropped record"
    );
    assert_eq!(
        st.owed_publications().expect("reads"),
        Owed::default(),
        "nothing owed holds the session's subjects"
    );
    assert!(st.set_post_seq("s-a", 3, 11, false).is_err());
    assert_eq!(
        st.post_seq("s-a", 3),
        None,
        "CONTROL: a refused reservation"
    );
    assert!(st.set_in_flight(&in_flight_entry(10)).is_err());
    assert!(
        st.in_flight().expect("reads").is_empty(),
        "CONTROL: a refused record in flight"
    );
    st.refuse_writes_of(None);
    st.set_in_flight(&in_flight_entry(12)).expect("in flight");
    st.refuse_writes_of(Some(journal::FILE));
    assert!(st.clear_in_flight(12).is_err());
    assert_eq!(
        st.in_flight().expect("reads"),
        vec![(12, in_flight_entry(12))],
        "CONTROL: a refused landing leaves its record in flight"
    );
    st.refuse_writes_of(None);
    st.clear_in_flight(12).expect("cleared again");
    st.set_post_seq("s-a", 2, 8, false).expect("reserved");
    let again = StateDir::open(&dir).expect("reopen");
    assert_eq!(
        again.post_seq("s-a", 1),
        None,
        "the journal was rewritten without it"
    );
    assert_eq!(again.post_seq("s-a", 2), Some((8, false)));
    assert!(again.in_flight().expect("reads").is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// ONE OLDER BUILD'S FILE THAT DOES NOT READ HOLDS ONLY ITS OWN POST (B19 review): a
/// `sent/<sid>.<id>` an older build left damaged is kept where it is and not taken in; that
/// post's reservation is refused naming the file, as that build refused it, while every other
/// post's reads, the records in flight read, and what is owed names its session; a repair that
/// removes it ends the hold. A damaged `inflight/<seq>` is refused naming it wherever the
/// records in flight are read (that build refused them all), and the reservations still read.
#[test]
fn one_older_builds_file_that_does_not_read_holds_only_its_own_post() {
    let dir = scratch("held");
    std::fs::create_dir_all(dir.join("sent")).expect("an older build's directory");
    std::fs::write(dir.join("sent/s-z.9"), "7 key identity\n").expect("a bug's damage");
    std::fs::write(dir.join("sent/s-a.4"), "17\n").expect("a whole one");
    let st = StateDir::open(&dir).expect("open");
    assert_eq!(st.post_pin("s-a", 1).expect("another post reads"), None);
    assert_eq!(
        st.post_seq("s-a", 4),
        Some((17, false)),
        "a whole file is taken in"
    );
    let held = st.post_pin("s-z", 9).expect_err("refused");
    assert!(
        held.to_string().contains("sent/s-z.9"),
        "names the file: {held}"
    );
    assert!(st.in_flight().expect("reads").is_empty());
    assert!(st.owed_publications().expect("reads").sids.contains("s-z"));
    assert!(
        dir.join("sent/s-z.9").exists(),
        "kept where it is for repair"
    );
    assert!(!dir.join("sent/s-a.4").exists());
    std::fs::remove_file(dir.join("sent/s-z.9")).expect("repaired");
    assert_eq!(st.post_pin("s-z", 9).expect("reads"), None, "the hold ends");
    std::fs::create_dir_all(dir.join("inflight")).expect("an older build's directory");
    std::fs::write(dir.join("inflight/5"), "not an entry\n").expect("a bug's damage");
    let next = StateDir::open(&dir).expect("reopen");
    let refused = next.in_flight().expect_err("refused");
    assert!(refused.to_string().contains("inflight/5"), "{refused}");
    assert!(next.owed_publications().is_err());
    assert_eq!(
        next.post_seq("s-a", 4),
        Some((17, false)),
        "the reservations read"
    );
    std::fs::remove_file(dir.join("inflight/5")).expect("repaired");
    assert!(next.in_flight().expect("reads").is_empty(), "the hold ends");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A JOURNAL AN OLDER BUILD'S ATTACH WENT PAST (B19 review): a bridge of this build stops
/// uncleanly with a post reserved and its record in flight, and a bridge of an older build
/// then attaches on the dir: it takes an incarnation and reads none of the journal. The next
/// open of this build retires the journal's record in flight into the quarantine, never to
/// be replayed, and keeps the reservation. CONTROL: this build's own restart in that place
/// (its attach named in the journal) replays the record, and so does the next open after a
/// stop whose hand-off the disk refused (the bridge then leaves the run open:
/// `Bridge::end_run`).
#[test]
fn a_journal_an_older_builds_attach_went_past_has_its_records_in_flight_retired() {
    let dir = scratch("stale-journal");
    let crashed = StateDir::open(&dir).expect("open");
    crashed
        .take_incarnation(0)
        .expect("an attach of this build");
    crashed.set_post_seq("s-a", 5, 70, false).expect("reserved");
    crashed
        .set_in_flight(&in_flight_entry(70))
        .expect("in flight");
    drop(crashed);
    let again = StateDir::open(&dir).expect("this build again");
    again.take_incarnation(0).expect("its attach");
    assert_eq!(
        again.in_flight().expect("reads").len(),
        1,
        "CONTROL: replayed"
    );
    assert!(again.take_stale_retired().is_empty());
    drop(again);
    let older = StateDir::open(&dir).expect("an older build's bridge");
    let mut lease = older.lease().expect("a lease");
    lease.used += 1;
    older
        .write("inc-lease", &lease.render())
        .expect("its attach, which names nothing in the journal");
    drop(older);
    let after = StateDir::open(&dir).expect("this build after it");
    assert!(after.in_flight().expect("reads").is_empty(), "retired");
    assert_eq!(
        after.post_seq("s-a", 5),
        Some((70, false)),
        "the reservation stays"
    );
    let retired = after.take_stale_retired();
    assert_eq!(
        retired.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(),
        ["inflight/70"]
    );
    assert_eq!(
        std::fs::read(dir.join(&retired[0].to)).expect("its copy"),
        format!("{}\n", in_flight_entry(70).render()).into_bytes()
    );
    after.take_incarnation(0).expect("this build's attach");
    after
        .set_in_flight(&in_flight_entry(71))
        .expect("in flight");
    after.refuse_writes_of(Some("inflight/71"));
    assert!(
        after.hand_posts_to_files().is_err(),
        "a hand-off the disk refuses"
    );
    drop(after);
    let next = StateDir::open(&dir).expect("the next bridge of this build");
    assert_eq!(
        next.in_flight().expect("reads"),
        vec![(71, in_flight_entry(71))],
        "CONTROL: the attach the journal names left it"
    );
    assert!(next.take_stale_retired().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A STOP'S HAND-OFF TAKEN BACK (B19 review): the next open of this build takes the files a
/// hand-off wrote back into the journal with barriers alone where they share its device (its
/// records on the media before the unlinks), never a full flush, and unlinks them and the
/// heads the hand-off kept beside them ([`journal::HANDED`], B05 review); a file
/// whose very record the journal holds already (a cut between the files and the journal's
/// unlink) is unlinked with no write at all. Each open reads the journal once, counted.
#[test]
fn a_hand_off_taken_back_costs_barriers_and_no_full_flush() {
    let dir = scratch("handback");
    let st = StateDir::open(&dir).expect("open");
    st.set_post_seq("s-a", 1, 7, false).expect("reserved");
    st.set_in_flight(&in_flight_entry(7)).expect("in flight");
    assert_eq!(st.hand_posts_to_files().expect("handed"), 2);
    assert!(!dir.join(journal::FILE).exists());
    drop(st);
    let next = StateDir::open(&dir).expect("the successor");
    let before = next.ops();
    assert_eq!(next.in_flight().expect("reads").len(), 1);
    let spent = next.ops() - before;
    assert_eq!(
        (spent.fsyncs, spent.unlinks, spent.appends),
        (0, 3, 2),
        "{spent:?}"
    );
    assert!(!dir.join(journal::HANDED).exists());
    assert!((1..=2).contains(&spent.barriers), "{spent:?}");
    assert!(!dir.join("sent/s-a.1").exists() && !dir.join("inflight/7").exists());
    next.write("sent/s-a.1", "7")
        .expect("a hand-off's file again");
    next.write("inflight/7", &in_flight_entry(7).render())
        .expect("a hand-off's file again");
    drop(next);
    let third = StateDir::open(&dir).expect("after a cut");
    let before = third.ops();
    assert_eq!(third.post_seq("s-a", 1), Some((7, false)));
    let spent = third.ops() - before;
    assert_eq!(
        (spent.fsyncs, spent.barriers, spent.appends, spent.unlinks),
        (0, 0, 0, 2),
        "{spent:?}"
    );
    let reads = third.ops().reads;
    assert_eq!(third.in_flight().expect("reads").len(), 1);
    assert_eq!(third.ops().reads, reads, "the journal is read once an open");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A receipt's pin file and a key's pin line hold no head (aterm-link B05 review): every
/// build reads them, and a build before the heads parses them strictly.
#[test]
fn a_receipts_and_a_keys_pin_hold_no_head() {
    let dir = scratch("pin-files");
    let st = StateDir::open(&dir).expect("open");
    let pin = PublishPin {
        at: Some(42),
        owner: Some(1),
        ..in_flight_entry(9).pin
    };
    st.set_receipt_pin("s-a", 3, &pin).expect("a receipt's pin");
    st.set_key_pin("s-a", "k", &pin).expect("a key's pin");
    for file in ["acks/s-a.3", "keys/s-a"] {
        let text = std::fs::read_to_string(dir.join(file)).expect("written");
        assert!(!text.contains(" at "), "{file}: {text}");
    }
    assert_eq!(
        st.receipt_pin("s-a", 3).expect("reads"),
        Some(pin.without_head())
    );
    assert_eq!(
        st.key_pin("s-a", "k").expect("reads"),
        Some(pin.without_head())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A STOP'S HAND-OFF KEEPS THE HEADS THE FILES CANNOT HOLD** (aterm-link B05 review): the
/// files it writes hold each reservation and record in flight as a build before the heads
/// parses them (no ` at `, no `at=`), and the next attach of this build takes their heads back
/// from what it kept beside them. CONTROL: once an attach went past the hand-off (an older
/// build's, which names nothing), the files are taken without heads, so their records are
/// published again at their pins rather than read back from a head that may no longer bound
/// only their own copies; and the kept heads go with the adoption.
#[test]
fn a_hand_off_keeps_the_heads_the_files_cannot_hold() {
    let headed = |seq: u64| InFlight {
        pin: PublishPin {
            at: Some(40),
            ..in_flight_entry(seq).pin
        },
        at: Some(50),
        ..in_flight_entry(seq)
    };
    for older_went_past in [false, true] {
        let dir = scratch("handed-heads");
        let st = StateDir::open(&dir).expect("open");
        st.take_incarnation(0).expect("an attach of this build");
        st.set_post_pin("s-a", 1, &headed(7).pin).expect("reserved");
        st.set_in_flight(&headed(7)).expect("in flight");
        assert_eq!(st.hand_posts_to_files().expect("handed"), 2);
        for file in ["sent/s-a.1", "inflight/7"] {
            let text = std::fs::read_to_string(dir.join(file)).expect("written");
            assert!(
                !text.contains(" at ") && !text.contains("at="),
                "{file}: {text}"
            );
        }
        assert_eq!(
            PublishPin::parse(&std::fs::read_to_string(dir.join("sent/s-a.1")).unwrap())
                .expect("as a build before the heads parses it"),
            headed(7).pin.without_head()
        );
        assert!(dir.join(journal::HANDED).exists());
        drop(st);
        if older_went_past {
            let older = StateDir::open(&dir).expect("an older build's bridge");
            let mut lease = older.lease().expect("a lease");
            lease.used += 1;
            older
                .write("inc-lease", &lease.render())
                .expect("its attach, which names nothing in the journal");
        }
        let next = StateDir::open(&dir).expect("the next bridge of this build");
        next.take_incarnation(0).expect("its attach");
        let (pin, entry) = if older_went_past {
            (headed(7).pin.without_head(), headed(7).without_head())
        } else {
            (headed(7).pin, headed(7))
        };
        assert_eq!(next.post_pin("s-a", 1).expect("reads"), Some(pin));
        assert_eq!(next.in_flight().expect("reads"), vec![(7, entry)]);
        assert!(!dir.join(journal::HANDED).exists(), "taken with the files");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// **A HAND-OFF WHOSE HEADS THE DISK REFUSES KEEPS THE JOURNAL** (item F4): the files are
/// written and the heads beside them refused, so the journal stays beside the files, as a
/// cut before its unlink leaves it, and the next attach of this build takes each record
/// with its head from it, to read back before it publishes it again. CONTROL (the journal
/// unlinked all the same, `hand_off_without_heads`): the records come from the files
/// without heads and are published again at their pins, which the sim's asking world found
/// putting a post on the bus twice past a cut that dropped its key.
#[test]
fn a_hand_off_whose_heads_the_disk_refuses_keeps_the_journal() {
    let headed = |seq: u64| InFlight {
        pin: PublishPin {
            at: Some(40),
            ..in_flight_entry(seq).pin
        },
        at: Some(50),
        ..in_flight_entry(seq)
    };
    for control in [false, true] {
        let dir = scratch("handed-refused");
        let st = StateDir::open(&dir).expect("open");
        st.take_incarnation(0).expect("an attach of this build");
        st.set_post_pin("s-a", 1, &headed(7).pin).expect("reserved");
        st.set_in_flight(&headed(7)).expect("in flight");
        st.hand_off_without_heads(control);
        st.refuse_writes_of(Some(journal::HANDED));
        assert_eq!(st.hand_posts_to_files().expect("handed"), 2);
        assert!(
            !dir.join(journal::HANDED).exists(),
            "the heads were refused"
        );
        assert_eq!(
            dir.join(journal::FILE).exists(),
            !control,
            "the journal stays beside the files only where it holds the heads alone"
        );
        drop(st);
        let next = StateDir::open(&dir).expect("the next bridge of this build");
        next.take_incarnation(0).expect("its attach");
        let (pin, entry) = if control {
            (headed(7).pin.without_head(), headed(7).without_head())
        } else {
            (headed(7).pin, headed(7))
        };
        assert_eq!(next.post_pin("s-a", 1).expect("reads"), Some(pin));
        assert_eq!(next.in_flight().expect("reads"), vec![(7, entry)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// **AN ATTACH TAKES A HAND-OFF'S FILES BEFORE ITS INCARNATION** (item F4): the hand-off's
/// records go into the journal with their heads as the next attach names itself, so one
/// that stops once it took its incarnation (its run record refused) leaves them, heads and
/// all, for the attach after it. CONTROL (`adopt_after_attach`, the files left to a later
/// read): the attach after finds the heads stale and takes the records without them, to be
/// published again at their pins unread.
#[test]
fn an_attach_takes_a_hand_offs_files_before_its_incarnation() {
    let headed = |seq: u64| InFlight {
        pin: PublishPin {
            at: Some(40),
            ..in_flight_entry(seq).pin
        },
        at: Some(50),
        ..in_flight_entry(seq)
    };
    for control in [false, true] {
        let dir = scratch("handed-attach");
        let st = StateDir::open(&dir).expect("open");
        st.take_incarnation(0).expect("an attach of this build");
        st.set_post_pin("s-a", 1, &headed(7).pin).expect("reserved");
        st.set_in_flight(&headed(7)).expect("in flight");
        assert_eq!(st.hand_posts_to_files().expect("handed"), 2);
        drop(st);
        let stopped = StateDir::open(&dir).expect("an attach that stops");
        stopped.adopt_after_attach(control);
        stopped.take_incarnation(0).expect("its incarnation");
        drop(stopped);
        let next = StateDir::open(&dir).expect("the attach after it");
        next.take_incarnation(0).expect("its attach");
        let (pin, entry) = if control {
            (headed(7).pin.without_head(), headed(7).without_head())
        } else {
            (headed(7).pin, headed(7))
        };
        assert_eq!(next.post_pin("s-a", 1).expect("reads"), Some(pin));
        assert_eq!(next.in_flight().expect("reads"), vec![(7, entry)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// **A HAND-OFF HANDED ON BEFORE ITS TAKER ATTACHED KEEPS ITS HEADS** (item F4): a bridge
/// that takes a hand-off's files into the journal and hands them on before its own attach
/// came up names the first hand-off's attach with them, so its own hand-off keeps their
/// heads under that attach and the next attach takes them. CONTROL
/// (`adopt_without_the_attach`): kept under no attach, the heads are refused and the records
/// taken without them.
#[test]
fn a_hand_off_handed_on_before_its_taker_attached_keeps_its_heads() {
    let headed = |seq: u64| InFlight {
        pin: PublishPin {
            at: Some(40),
            ..in_flight_entry(seq).pin
        },
        at: Some(50),
        ..in_flight_entry(seq)
    };
    for control in [false, true] {
        let dir = scratch("handed-on");
        let st = StateDir::open(&dir).expect("open");
        st.take_incarnation(0).expect("an attach of this build");
        st.set_post_pin("s-a", 1, &headed(7).pin).expect("reserved");
        st.set_in_flight(&headed(7)).expect("in flight");
        assert_eq!(st.hand_posts_to_files().expect("handed"), 2);
        drop(st);
        let middle = StateDir::open(&dir).expect("a bridge whose attach never comes up");
        middle.adopt_without_the_attach(control);
        assert_eq!(
            middle.in_flight().expect("taken in"),
            vec![(7, headed(7))],
            "PRECONDITION: the middle bridge takes the heads"
        );
        assert_eq!(middle.hand_posts_to_files().expect("handed on"), 2);
        drop(middle);
        let next = StateDir::open(&dir).expect("the next bridge");
        next.take_incarnation(0).expect("its attach");
        let (pin, entry) = if control {
            (headed(7).pin.without_head(), headed(7).without_head())
        } else {
            (headed(7).pin, headed(7))
        };
        assert_eq!(next.post_pin("s-a", 1).expect("reads"), Some(pin));
        assert_eq!(next.in_flight().expect("reads"), vec![(7, entry)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// THE SWEEP SALVAGES A DAMAGED JOURNAL (B19 review): after an unclean stop in an earlier
/// boot, a journal with damage in its first reservation keeps every whole record after it, so the
/// reservation after the damage still reads and the record in flight after it is retired as
/// its own quarantine copy, not left only inside the journal's raw bytes.
#[test]
fn the_sweep_keeps_every_whole_record_after_damage_in_the_journal() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("salvage")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = dir.path();
    let st = StateDir::open(root).expect("open");
    st.set_inc_seam(in_boot(1));
    let inc = st.take_incarnation(0).expect("the last attach, in boot 1");
    st.set_post_seq("s-a", 1, 7, false).expect("reserved");
    st.set_in_flight(&in_flight_entry(7)).expect("in flight");
    st.clear_in_flight(7).expect("landed");
    st.set_post_seq("s-a", 2, 8, false).expect("reserved");
    st.set_in_flight(&in_flight_entry(8)).expect("in flight");
    drop(st);
    let path = root.join(journal::FILE);
    let mut damaged = std::fs::read(&path).expect("the journal");
    let named = journal::encode(0, &journal::Frame::Attached(inc)).expect("a record");
    damaged[named.len() + 20] ^= 0x01;
    std::fs::write(&path, &damaged).expect("damage in its first reservation");
    let st = StateDir::open(root).expect("boot 2's bridge");
    st.set_inc_seam(in_boot(2));
    let moved = st.set_aside_torn().expect("the sweep");
    assert_eq!(
        moved.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(),
        [journal::FILE]
    );
    assert_eq!(
        std::fs::read(root.join("quarantine/journal.posts")).expect("kept"),
        damaged
    );
    assert_eq!(st.post_seq("s-a", 2), Some((8, false)), "after the damage");
    assert_eq!(
        st.post_seq("s-a", 1),
        None,
        "the damaged record's reservation"
    );
    let retired = st
        .retire_in_flight_of_an_earlier_boot()
        .expect("the retirement");
    assert_eq!(
        retired.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(),
        ["inflight/8"]
    );
    assert!(root.join(&retired[0].to).exists());
    assert!(st.in_flight().expect("reads").is_empty());
}

#[test]
fn a_posts_sequence_is_pinned_to_the_post_and_survives_a_reopen() {
    let dir = scratch("postseq");
    let st = StateDir::open(&dir).expect("open");
    assert_eq!(st.post_seq("s-a", 1), None);
    st.set_post_seq("s-a", 1, 0x0000_0002_0000_0007, false)
        .expect("reserve");
    assert_eq!(
        StateDir::open(&dir).expect("reopen").post_seq("s-a", 1),
        Some((0x0000_0002_0000_0007, false))
    );
    assert_eq!(st.post_seq("s-a", 2), None);
    st.clear_post_seq("s-a", 1);
    assert_eq!(st.post_seq("s-a", 1), None);
    st.set_post_seq("s-a", 3, 0x0000_0002_0000_0009, true)
        .expect("reserve via key");
    assert_eq!(
        StateDir::open(&dir).expect("reopen").post_seq("s-a", 3),
        Some((0x0000_0002_0000_0009, true))
    );
    assert!(
        !dir.join("sent").exists(),
        "a reservation is a record of the journal, not a file"
    );
    std::fs::create_dir_all(dir.join("sent")).expect("an older build's directory");
    std::fs::write(dir.join("sent/s-a.4"), "17\n").expect("write");
    let adopting = StateDir::open(&dir).expect("reopen");
    assert_eq!(adopting.post_seq("s-a", 4), Some((17, false)));
    assert!(
        !dir.join("sent/s-a.4").exists(),
        "an older build's reservation is taken into the journal"
    );
    assert_eq!(
        StateDir::open(&dir).expect("reopen").post_seq("s-a", 4),
        Some((17, false)),
        "and stays there"
    );
    assert_eq!(
        adopting.post_seq("s-a", 3),
        Some((0x0000_0002_0000_0009, true))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_receipt_pins_its_sequence_until_it_is_retired() {
    let dir = scratch("acks");
    let st = StateDir::open(&dir).expect("open");
    assert_eq!(st.receipt_seq("s-a", 1), None);
    st.set_receipt_seq("s-a", 1, 77).expect("reserve");
    st.set_receipt_seq("s-a", 2, 78).expect("reserve");
    st.set_receipt_seq("s-b", 1, 79).expect("reserve");
    let again = StateDir::open(&dir).expect("reopen");
    assert_eq!(again.receipt_seq("s-a", 1), Some(77), "survives a reopen");
    assert_eq!(again.receipt_seq("s-a", 2), Some(78));
    assert_eq!(again.receipt_seq("s-b", 1), Some(79), "per session");
    again.clear_receipt_seq("s-a", 1);
    assert_eq!(again.receipt_seq("s-a", 1), None, "retired");
    assert_eq!(again.receipt_seq("s-a", 2), Some(78), "the others stand");
    assert!(
        std::fs::read_to_string(dir.join("acks/s-a.2"))
            .expect("the pin file")
            .trim()
            .parse::<u64>()
            .is_ok(),
        "a pin holds the number and nothing else"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A RETIRED FILE LEAVES THE SET THE CLEAN STOP FLUSHES** (B20): N posts, each landed and
/// retired with its receipt, by N sessions whose refill floors went with them, leave the set
/// the clean stop stats and flushes as one does: the post journal alone, so the stop's
/// work does not grow with N. CONTROL: while a receipt's reservation and a session's floor
/// stand, the set names them.
#[test]
fn retired_files_leave_the_set_the_clean_stop_flushes() {
    let after = |n: u64| {
        let dir = aterm_tempfile::Builder::new()
            .prefix("unflushed")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let st = StateDir::open(dir.path()).expect("open");
        st.set_inc_seam(in_boot(1));
        st.begin_run().expect("an open run");
        for i in 1..=n {
            let sid = format!("s-{i}");
            let entry = in_flight_entry(100 + i);
            st.set_post_pin(&sid, i, &entry.pin).expect("a reservation");
            st.set_in_flight(&entry).expect("in flight");
            st.set_receipt_pin(&sid, i, &entry.pin)
                .expect("a receipt's reservation");
            st.set_topic_floors(&sid, &BTreeMap::from([("t".to_string(), i)]))
                .expect("a refill floor");
            let held = st.unflushed.lock().unwrap().clone();
            assert!(
                held.contains(&format!("acks/{sid}.{i}"))
                    && held.contains(&format!("unhandled/{sid}")),
                "CONTROL: what stands is in the set: {held:?}"
            );
            st.clear_in_flight(entry.pin.seq).expect("landed");
            st.clear_post_seq(&sid, i);
            st.clear_receipt_seq(&sid, i);
            st.forget_topics(&sid);
        }
        let pending = st.unflushed.lock().unwrap().clone();
        let flushed = st.end_run_clean().expect("a clean stop");
        (pending, flushed.files, flushed.dirs)
    };
    let one = after(1);
    assert_eq!(
        one.0,
        BTreeSet::from([journal::FILE.to_string()]),
        "the journal alone stands"
    );
    assert_eq!(
        after(64),
        one,
        "64 retired posts and receipts leave the stop what one leaves"
    );
}

/// **AN EARLIER BOOT'S SESSIONS' FILES AND RESERVATIONS GO, AND THIS BOOT'S STAY** (B20,
/// item C14): a run writes every per-session file for two sessions, reserves a post of each
/// and of a third whose record stays in flight, then stops cleanly or not. A start in the
/// next boot, judged before its run record names that boot, finds an earlier boot, and the
/// sweep unlinks every such file of a sid the roster does not name and retires each post
/// reservation of such a sid, so that session is owed no longer
/// ([`StateDir::owed_publications`]); it keeps a live sid's files and reservation, a file
/// that does not read whole, and a reservation whose sequence a record in flight still
/// holds (after a clean stop; an unclean one retired that record first), and it touches no
/// record in flight. A start in the same boot finds no earlier boot and keeps all three. A
/// dir with no run record is judged by its lease's boot. OFF THE BOOT VOLUME (item RZ2) the
/// same holds, the lease naming the kernel's boot there too, with a run record and without.
/// AN ATTACH OF THIS BOOT WINS OVER THE RECORD: an older build attaches without writing
/// `run`, so a lease that names this boot says its sessions may still run, and one an older
/// build wrote off the boot volume, which names no boot, cannot rule one out; neither is an
/// earlier boot, and the older build's session keeps its file. CONTROL (RZ2,
/// `lease_names_no_boot_off_the_boot_volume`): boot 1's lease written as before RZ2 names no
/// boot off the boot volume, and the next boot finds no earlier boot there and sweeps
/// nothing, while on the boot volume the same write names its boot and is swept. CONTROL
/// (`keep_earlier_boot_reservations`): a sweep that retires no reservation leaves the ended
/// session owed, its files gone.
#[test]
fn an_earlier_boots_sessions_files_are_swept_and_this_boots_kept() {
    let session_files = |sid: &str| {
        [
            format!("keys/{sid}"),
            format!("acks/{sid}.3"),
            format!("unhandled/{sid}"),
            format!("seen/{sid}"),
            format!("topics/{sid}"),
        ]
    };
    // How the dir stands between boot 1's run and the next start.
    #[derive(Clone, Copy, PartialEq)]
    enum Then {
        Nothing,
        // The run record is gone.
        NoRecord,
        // An older build, which writes no run record, attached in the next start's boot
        // and keyed a post of its session.
        OlderAttach,
    }
    // Whether the record in flight still held the third reservation, at each sweep that
    // retires.
    let mut met: BTreeSet<bool> = BTreeSet::new();
    // (case, a clean stop, then, off the boot volume, boot 1's lease an older build's (it
    // names only a boot whose page cache vouches for the dir: the RZ2 control), the next
    // start's boot, an earlier boot, the sweep keeps every reservation: the C14 control)
    for (case, clean, then, off_volume, legacy, next_boot, want, keep) in [
        (
            "a clean stop, then a reboot",
            true,
            Then::Nothing,
            false,
            false,
            2,
            true,
            false,
        ),
        (
            "an unclean stop, then a reboot",
            false,
            Then::Nothing,
            false,
            false,
            2,
            true,
            false,
        ),
        (
            "no run record, then a reboot",
            false,
            Then::NoRecord,
            false,
            false,
            2,
            true,
            false,
        ),
        (
            "a clean stop, then a start in the same boot",
            true,
            Then::Nothing,
            false,
            false,
            1,
            false,
            false,
        ),
        (
            "an open run, then a start in the same boot",
            false,
            Then::Nothing,
            false,
            false,
            1,
            false,
            false,
        ),
        (
            "a clean stop, then a reboot and an older build's attach before the start",
            true,
            Then::OlderAttach,
            false,
            false,
            2,
            false,
            false,
        ),
        (
            "a clean stop off the boot volume, then a reboot",
            true,
            Then::Nothing,
            true,
            false,
            2,
            true,
            false,
        ),
        (
            "no run record off the boot volume, then a reboot",
            false,
            Then::NoRecord,
            true,
            false,
            2,
            true,
            false,
        ),
        (
            "a clean stop off the boot volume, then a start in the same boot",
            true,
            Then::Nothing,
            true,
            false,
            1,
            false,
            false,
        ),
        (
            "a clean stop off the boot volume, then a reboot and an older build's attach, \
             whose lease names no boot, before the start",
            true,
            Then::OlderAttach,
            true,
            false,
            2,
            false,
            false,
        ),
        (
            "CONTROL (RZ2): a clean stop off the boot volume under an older build's lease, \
             which names no boot, then a reboot",
            true,
            Then::Nothing,
            true,
            true,
            2,
            false,
            false,
        ),
        (
            "the same lease on the boot volume, where it names its boot",
            true,
            Then::Nothing,
            false,
            true,
            2,
            true,
            false,
        ),
        (
            "CONTROL: an unclean stop, then a reboot, and a sweep that keeps every reservation",
            false,
            Then::Nothing,
            false,
            false,
            2,
            true,
            true,
        ),
    ] {
        let seam = |boot| IncSeam {
            off_volume,
            ..in_boot(boot)
        };
        let dir = aterm_tempfile::Builder::new()
            .prefix("earlier-boot")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(seam(1));
        st.lease_names_no_boot_off_the_boot_volume(legacy);
        st.begin_run().expect("boot 1's run");
        st.take_incarnation(0).expect("boot 1's lease");
        assert_eq!(
            st.lease().and_then(|lease| lease.boot),
            (!(legacy && off_volume)).then(|| boot_id(1)),
            "{case}: the boot boot 1's lease names"
        );
        for sid in ["s-gone", "s-live"] {
            st.set_key_seq(sid, "k", 9).expect("a key");
            st.set_receipt_seq(sid, 3, 10).expect("a receipt's");
            st.set_topic_floors(sid, &BTreeMap::from([("t".to_string(), 4)]))
                .expect("a floor");
            st.set_seen_off(sid, 5).expect("seen");
            st.set_topics(
                sid,
                &BTreeMap::from([("t".to_string(), TopicCursor { next: 6 })]),
            )
            .expect("a cursor");
        }
        st.set_receipt_seq("s-torn", 1, 11).expect("a receipt's");
        std::fs::write(root.join("acks/s-torn.1"), b"1").expect("tear it");
        // A post of a session that boot ended, one of a live session, and one of an ended
        // session whose record is in flight, tied to it by its pin's sequence alone.
        let entry = in_flight_entry(12);
        for (sid, seq) in [("s-gone", 14), ("s-live", 15)] {
            st.set_post_pin(sid, 1, &in_flight_entry(seq).pin)
                .expect("a reservation");
        }
        st.set_post_pin("s-held", 1, &entry.pin)
            .expect("a reservation");
        st.set_in_flight(&entry).expect("in flight");
        if clean {
            st.end_run_clean().expect("its clean stop");
        }
        drop(st);
        match then {
            Then::Nothing => {}
            Then::NoRecord => std::fs::remove_file(root.join("run")).expect("no record"),
            Then::OlderAttach => {
                let older = StateDir::open(root).expect("an older build's open");
                older.set_inc_seam(seam(next_boot));
                older.lease_names_no_boot_off_the_boot_volume(true);
                older
                    .take_incarnation(0)
                    .expect("its attach, which writes no run record");
                older
                    .set_key_seq("s-older", "k", 13)
                    .expect("its session's key");
            }
        }
        let next = StateDir::open(root).expect("the next start");
        next.set_inc_seam(seam(next_boot));
        let earlier = next.last_ran_in_an_earlier_boot();
        assert_eq!(earlier, want, "{case}: an earlier boot");
        next.set_aside_torn().expect("the sweep of torn files");
        next.retire_in_flight_of_an_earlier_boot()
            .expect("the retirement");
        next.begin_run().expect("the next run");
        assert!(
            !next.last_ran_in_an_earlier_boot(),
            "{case}: once the record names this boot, never an earlier one"
        );
        let torn_left = root.join("acks/s-torn.1").exists();
        let reserved = |st: &StateDir| {
            ["s-gone", "s-live", "s-held"].map(|sid| st.post_seq(sid, 1).map(|(seq, _)| seq))
        };
        assert_eq!(
            reserved(&next),
            [Some(14), Some(15), Some(12)],
            "{case}: the three reservations"
        );
        let in_flight = next.in_flight().expect("the records in flight");
        // An unclean stop in an earlier boot retired the record in flight before the sweep.
        let held = in_flight.iter().any(|(seq, _)| *seq == 12);
        let owed_before = next.owed_publications().expect("what is owed").sids;
        assert!(
            ["s-gone", "s-live", "s-held"]
                .iter()
                .all(|sid| owed_before.contains(*sid)),
            "{case}: every reserved session is owed: {owed_before:?}"
        );
        next.keep_earlier_boot_reservations(keep);
        let removed = if earlier {
            next.sweep_sessions_of_an_earlier_boot(&BTreeSet::from(["s-live".to_string()]))
                .expect("the sweep")
        } else {
            0
        };
        let swept = earlier && !keep;
        if swept {
            met.insert(held);
        }
        let retired: BTreeSet<String> = [("s-gone", swept), ("s-held", swept && !held)]
            .into_iter()
            .filter(|(_, gone)| *gone)
            .map(|(sid, _)| sid.to_string())
            .collect();
        for rel in session_files("s-gone") {
            assert_eq!(
                root.join(&rel).exists(),
                !earlier,
                "{case}: {rel} of a session that boot ended"
            );
            assert!(
                !next.unflushed.lock().unwrap().contains(&rel) || !earlier,
                "{case}: an unlinked {rel} is not flushed in place"
            );
        }
        for rel in session_files("s-live") {
            assert!(root.join(&rel).exists(), "{case}: {rel} of a live sid");
        }
        assert_eq!(
            root.join("keys/s-older").exists(),
            then == Then::OlderAttach,
            "{case}: the older build's session keeps its key"
        );
        assert_eq!(
            removed,
            (if earlier { 5 } else { 0 }) + retired.len() as u64,
            "{case}"
        );
        assert_eq!(
            root.join("acks/s-torn.1").exists(),
            torn_left,
            "{case}: a file that does not read whole is left to the sweep of torn files"
        );
        assert_eq!(
            reserved(&next),
            [
                (!swept).then_some(14),
                Some(15),
                (!retired.contains("s-held")).then_some(12)
            ],
            "{case}: an ended session's reservation retired, a live one's and one a record \
             in flight holds kept"
        );
        assert_eq!(
            next.in_flight().expect("the records in flight"),
            in_flight,
            "{case}: no record in flight is the sweep's"
        );
        let owed_after = next.owed_publications().expect("what is owed").sids;
        assert_eq!(
            owed_after,
            owed_before
                .difference(&retired)
                .cloned()
                .collect::<BTreeSet<String>>(),
            "{case}: only the sessions whose every reservation the sweep retired are owed \
             no longer"
        );
        if keep {
            assert!(
                owed_after.contains("s-gone") && !root.join("acks/s-gone.3").exists(),
                "CONTROL: without the retire the ended session stays owed, its files gone"
            );
        }
        let reread = StateDir::peek(root);
        reread.set_inc_seam(seam(next_boot));
        assert_eq!(
            reserved(&reread),
            reserved(&next),
            "{case}: the retires are in the journal"
        );
    }
    assert_eq!(
        met,
        BTreeSet::from([false, true]),
        "the sweep met the third reservation both with its record in flight and with it retired"
    );
}

/// **ONE REFUSAL STOPS NONE OF THE REST OF AN EARLIER BOOT'S SWEEP**: a place the sweep
/// cannot list (`seen` a file where its directory should be, listed before `topics`) is
/// the error the sweep returns, and the ended session's files in every other place are
/// unlinked and its post reservation retired all the same, so it is owed no longer. A
/// sweep that stopped at the refusal would leave its reservation charged at every boot
/// the refusal repeats. CONTROL: with `seen` a directory, the same sweep returns the count.
#[test]
fn one_refusal_stops_none_of_the_rest_of_an_earlier_boots_sweep() {
    for refuse in [true, false] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("earlier-boot-refused")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(in_boot(1));
        st.begin_run().expect("boot 1's run");
        st.take_incarnation(0).expect("boot 1's lease");
        st.set_key_seq("s-gone", "k", 9).expect("a key");
        st.set_receipt_seq("s-gone", 3, 10).expect("a receipt's");
        st.set_topics(
            "s-gone",
            &BTreeMap::from([("t".to_string(), TopicCursor { next: 6 })]),
        )
        .expect("a cursor");
        st.set_post_pin("s-gone", 1, &in_flight_entry(14).pin)
            .expect("a reservation");
        st.end_run_clean().expect("its clean stop");
        drop(st);
        let next = StateDir::open(root).expect("the next start");
        next.set_inc_seam(in_boot(2));
        assert!(next.last_ran_in_an_earlier_boot(), "an earlier boot");
        next.begin_run().expect("the next run");
        if refuse {
            // The open made `seen/`; what replaced it since does not list.
            let seen = root.join("seen");
            std::fs::remove_dir(&seen).expect("an empty seen/");
            std::fs::write(&seen, b"").expect("a file where seen/ should be");
        }
        let swept = next.sweep_sessions_of_an_earlier_boot(&BTreeSet::new());
        if refuse {
            let e = swept.expect_err("the place that does not list");
            assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
        } else {
            assert_eq!(
                swept.expect("CONTROL: the sweep"),
                4,
                "three files, one post"
            );
        }
        for rel in ["keys/s-gone", "acks/s-gone.3", "topics/s-gone"] {
            assert!(!root.join(rel).exists(), "refused={refuse}: {rel} unlinked");
        }
        assert_eq!(
            next.post_seq("s-gone", 1),
            None,
            "refused={refuse}: the reservation retired"
        );
        assert!(
            !next
                .owed_publications()
                .expect("what is owed")
                .sids
                .contains("s-gone"),
            "refused={refuse}: the ended session is owed no longer"
        );
    }
}

#[test]
fn publication_identity_survives_pins_and_malformed_reservations_are_not_absent() {
    let dir = scratch("identities");
    let st = StateDir::open(&dir).unwrap();
    let pin = PublishPin {
        seq: 17,
        via_key: true,
        identity: Some(PublishIdentity {
            subject: "/f/f/in/n-a/s-b/n-c/ask".into(),
            digest: "a".repeat(64),
        }),
        original: Some(PublishIdentity {
            subject: "/f/f/in/n-d/s-e/n-c/note".into(),
            digest: "b".repeat(64),
        }),
        original_off: Some(0),
        owner: Some(1),
        at: Some(42),
    };
    st.set_post_pin("s-a", 1, &pin).unwrap();
    assert_eq!(
        StateDir::open(&dir).unwrap().post_pin("s-a", 1).unwrap(),
        Some(pin.clone())
    );
    for damaged in [
        "",
        "17 identity /f/f/x nope",
        "17 identity /f/f/x",
        "17 garbage",
        "17 owner 1 owner 2",
        "17 original-off nope",
        "17 at nope",
        "17 at 1 at 2",
    ] {
        st.write("sent/s-a.1", damaged).unwrap();
        assert_eq!(
            StateDir::open(&dir)
                .unwrap()
                .post_pin("s-a", 1)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData,
            "an older build's damaged reservation {damaged:?}"
        );
        assert!(dir.join("sent/s-a.1").exists(), "kept for repair");
    }
    std::fs::remove_file(dir.join("sent/s-a.1")).unwrap();
    assert_eq!(
        StateDir::open(&dir).unwrap().post_pin("s-a", 1).unwrap(),
        Some(pin.clone()),
        "CONTROL: the journal's reservation"
    );
    let journal = dir.join(journal::FILE);
    let mut bytes = std::fs::read(&journal).unwrap();
    bytes.extend(
        journal::encode(
            1,
            &journal::Frame::Retire {
                sid: "s-a".into(),
                id: 1,
            },
        )
        .unwrap(),
    );
    bytes[20] ^= 0x01;
    std::fs::write(&journal, &bytes).unwrap();
    let err = StateDir::open(&dir)
        .unwrap()
        .post_pin("s-a", 1)
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData, "damage: {err}");
    assert_eq!(std::fs::read(&journal).unwrap(), bytes, "kept for repair");
    let mut cut = bytes.clone();
    cut[20] ^= 0x01;
    cut.truncate(cut.len() - 3);
    std::fs::write(&journal, &cut).unwrap();
    let healed = StateDir::open(&dir).unwrap();
    assert_eq!(
        healed.post_pin("s-a", 1).unwrap(),
        Some(pin.clone()),
        "a torn tail is what an append that failed part-way leaves: the records before it"
    );
    healed.clear_post_seq("s-a", 1);
    assert_eq!(
        StateDir::open(&dir).unwrap().post_pin("s-a", 1).unwrap(),
        None,
        "the next record goes after the tail is gone"
    );
    for damaged in [
        "key-only",
        "key-only garbage",
        "key-only 17 identity /f/f/x bad",
    ] {
        st.write("keys/s-a", damaged).unwrap();
        assert_eq!(
            st.key_pin("s-a", "key-only").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
    st.write("keys/s-a", "key-only\t17").unwrap();
    assert_eq!(st.key_pin("s-a", "key-only").unwrap().unwrap().seq, 17);
    st.forget_key_seq("s-a", "key-only", 17).unwrap();
    assert!(st.key_pin("s-a", "key-only").unwrap().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn in_flight_entries_round_trip_in_order_and_a_damaged_one_is_not_absent() {
    let dir = scratch("inflight");
    let st = StateDir::open(&dir).unwrap();
    assert!(
        st.in_flight().unwrap().is_empty(),
        "no dir is nothing in flight"
    );
    let entry = |seq: u64, body: &[u8]| InFlight {
        pin: PublishPin {
            seq,
            via_key: false,
            identity: Some(PublishIdentity {
                subject: "/f/f/in/n-a/s-b/n-a/note".into(),
                digest: "c".repeat(64),
            }),
            original: None,
            original_off: None,
            owner: Some(3),
            at: None,
        },
        subject: "/f/f/in/n-a/s-b/n-a/note".into(),
        body: body.to_vec(),
        log: None,
        at: None,
    };
    let late = InFlight {
        log: Some(astream_broker::LogIdentity {
            id: [0xaa; 16],
            generation: 2,
        }),
        ..entry(0x0000_0002_0000_0009, b"v=1 t=7 text=two%0Alines\n\x00\xff")
    };
    let early = entry(0x0000_0002_0000_0002, b"");
    st.set_in_flight(&late).unwrap();
    st.set_in_flight(&early).unwrap();
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(dir.join(journal::FILE))
            .unwrap()
            .permissions(),
    );
    assert_eq!(
        mode & 0o077,
        0,
        "a record's bytes are the owner's: {mode:o}"
    );
    assert_eq!(
        StateDir::open(&dir).unwrap().in_flight().unwrap(),
        vec![(early.pin.seq, early.clone()), (late.pin.seq, late.clone())]
    );
    st.clear_in_flight(early.pin.seq).unwrap();
    std::fs::create_dir_all(dir.join("inflight")).unwrap();
    std::fs::write(dir.join("inflight/.9.tmp.1.abcd"), "junk").unwrap();
    st.write(&format!("inflight/{}", early.pin.seq), &early.render())
        .unwrap();
    assert_eq!(
        StateDir::open(&dir).unwrap().in_flight().unwrap(),
        vec![(early.pin.seq, early.clone()), (late.pin.seq, late.clone())],
        "an older build's entry is taken into the journal, a temp left alone"
    );
    assert!(!dir.join(format!("inflight/{}", early.pin.seq)).exists());
    for damaged in [
        "",
        "8589934601",
        "8589934601\n/f/f/x",
        "8589934601\n/f/f/x\nnot-hex",
        "8589934601\n/f/f/*\n00",
        "8589934601\n/f/f/x\n00\nextra",
        "8589934601\n/f/f/x\n00\nlog=",
        "8589934601\n/f/f/x\n00\nlog=AA.0",
        "8589934601\n/f/f/x\n00\nlog=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.0\nextra",
        "8589934600\n/f/f/x\n00",
    ] {
        st.write(&format!("inflight/{}", late.pin.seq), damaged)
            .unwrap();
        assert_eq!(
            StateDir::open(&dir)
                .unwrap()
                .in_flight()
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData,
            "{damaged:?} must block recovery"
        );
        assert!(
            dir.join(format!("inflight/{}", late.pin.seq)).exists(),
            "uncertain publication fate must not discard its journal"
        );
    }
    std::fs::remove_file(dir.join(format!("inflight/{}", late.pin.seq))).unwrap();
    let st = StateDir::open(&dir).unwrap();
    st.clear_in_flight(late.pin.seq).unwrap();
    st.clear_in_flight(early.pin.seq).unwrap();
    assert!(st.in_flight().unwrap().is_empty());
    assert!(StateDir::open(&dir)
        .unwrap()
        .in_flight()
        .unwrap()
        .is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_acked_log_is_appended_reads_an_older_file_and_never_takes_a_torn_tail() {
    let dir = scratch("acked-log");
    let st = StateDir::open(&dir).unwrap();
    let set = |offs: &[u64]| {
        offs.iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(
        st.self_acked_log(),
        AckedLog {
            appendable: true,
            ..AckedLog::default()
        },
        "no file is the empty log, appendable"
    );
    for (i, off) in [5, 7, 9, 7].into_iter().enumerate() {
        st.append_self_acked((i == 0).then_some(0), &[off]).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("acked")).unwrap(),
        "floor=0,5,7,9,7,",
        "the first append into a file that names no floor names it"
    );
    assert_eq!(
        st.self_acked_log(),
        AckedLog {
            offsets: set(&[5, 7, 9]),
            floor: Some(0),
            entries: 4,
            appendable: true,
        }
    );
    let older = |text: &str| {
        std::fs::write(dir.join("acked"), text).unwrap();
        st.self_acked_log()
    };
    assert_eq!(
        older("1,2,3\n"),
        AckedLog {
            offsets: set(&[1, 2, 3]),
            floor: None,
            entries: 3,
            appendable: false,
        },
        "an older bridge's line"
    );
    assert_eq!(
        older("5,7,45"),
        AckedLog {
            offsets: set(&[5, 7]),
            floor: None,
            entries: 2,
            appendable: false,
        },
        "45 may be the torn head of 456: it is no offset of ours"
    );
    assert_eq!(older("").offsets, set(&[]));
    assert_eq!(
        older("4,floor=3,5,"),
        AckedLog {
            offsets: set(&[4, 5]),
            floor: Some(3),
            entries: 2,
            appendable: true,
        },
        "the floor entry is no offset and no entry"
    );
    st.set_self_acked(&set(&[20, 30, 40]), 10, 2).unwrap();
    let text = std::fs::read_to_string(dir.join("acked")).unwrap();
    assert_eq!(
        text, "floor=30,30,40,",
        "the newest two, each ended by its comma, under a floor raised to the oldest kept"
    );
    let older_reader: std::collections::BTreeSet<u64> = text
        .trim()
        .split(',')
        .filter_map(|n| n.parse().ok())
        .collect();
    assert_eq!(
        older_reader,
        set(&[30, 40]),
        "an older bridge reads the same set"
    );
    assert!(st.self_acked_log().appendable);
    std::fs::remove_file(dir.join("acked")).unwrap();
    let elsewhere = dir.join("elsewhere");
    std::fs::write(&elsewhere, "").unwrap();
    std::os::unix::fs::symlink(&elsewhere, dir.join("acked")).unwrap();
    assert!(
        st.append_self_acked(None, &[1]).is_err(),
        "a planted link is refused"
    );
    assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_acked_append_preserves_a_file_with_another_name() {
    let scratch = crate::test_scratch_dir("acked");
    let root = std::fs::canonicalize(scratch.path()).expect("canonical state scratch");
    let ordinary = StateDir::open(root.join("ordinary")).expect("ordinary state");
    ordinary
        .append_self_acked(None, &[5])
        .expect("first ordinary append");
    ordinary
        .append_self_acked(None, &[7])
        .expect("second ordinary append");
    assert_eq!(
        std::fs::read(ordinary.path("acked")).expect("ordinary log"),
        b"5,7,",
        "ordinary append controls precede the occupied-file case"
    );

    let linked = StateDir::open(root.join("linked")).expect("linked state");
    let other = root.join("other-owned-file");
    const SENTINEL: &[u8] = b"the other file owns these bytes\n";
    std::fs::write(&other, SENTINEL).expect("owned sentinel");
    std::fs::hard_link(&other, linked.path("acked")).expect("owned hardlink");
    let result = linked.append_self_acked(None, &[42]);
    let other_bytes = std::fs::read(&other).expect("other file remains readable");
    let state_bytes = std::fs::read(linked.path("acked")).expect("state name remains readable");
    assert!(
        result.is_err() && other_bytes == SENTINEL && state_bytes == SENTINEL,
        "ACKED_APPEND_REQUIRES_OWNED_SINGLE_LINK_FILE: result={result:?}, other={other_bytes:?}, state={state_bytes:?}"
    );
}

/// **AN ACK APPENDED AFTER A REPLACE LANDS IN THE FILE `acked` NAMES** (D05): the log is held
/// open from its first append, so appends after it open nothing, and a replace
/// (`set_self_acked`'s rename) lets the descriptor go, so the next append opens the file the
/// replace put at the name, refused there as at the first open: a hard link planted after a
/// replace takes no ack. NEGATIVE CONTROL (`keep_acked_across_its_replace`): a descriptor
/// kept across the replace writes the ack into the file the rename unlinked, and a read of
/// `acked` misses it.
#[test]
fn an_ack_appended_after_a_replace_lands_in_the_file_acked_names() {
    let set = |offs: &[u64]| {
        offs.iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
    };
    let scratch = crate::test_scratch_dir("acked");
    let root = std::fs::canonicalize(scratch.path()).expect("canonical state scratch");
    for keeps in [false, true] {
        let st = StateDir::open(root.join(format!("keeps-{keeps}"))).expect("state");
        let before = st.ops();
        for off in [1, 2, 3] {
            st.append_self_acked(None, &[off]).expect("append");
        }
        assert_eq!(
            (st.ops() - before).opens,
            1,
            "keeps={keeps}: three appends, one open"
        );
        st.keep_acked_across_its_replace(keeps);
        st.set_self_acked(&set(&[2, 3]), 0, 8).expect("replace");
        let before = st.ops();
        st.append_self_acked(None, &[4])
            .expect("the append after the replace");
        let (opens, read) = ((st.ops() - before).opens, st.self_acked_log().offsets);
        if keeps {
            assert_eq!(
                (opens, read),
                (0, set(&[2, 3])),
                "NEGATIVE CONTROL: the kept descriptor took 4 into the unlinked file"
            );
            continue;
        }
        assert_eq!(
            (opens, read),
            (1, set(&[2, 3, 4])),
            "4 is in the file at the name"
        );
        assert_eq!(
            std::fs::read_to_string(st.path("acked")).expect("acked"),
            "floor=0,2,3,4,"
        );

        st.set_self_acked(&set(&[4]), 0, 8)
            .expect("a second replace");
        let other = root.join("other-owned-file");
        const SENTINEL: &[u8] = b"the other file owns these bytes\n";
        std::fs::write(&other, SENTINEL).expect("owned sentinel");
        std::fs::remove_file(st.path("acked")).expect("unlink the replaced log");
        std::fs::hard_link(&other, st.path("acked")).expect("owned hardlink");
        let result = st.append_self_acked(None, &[5]);
        assert!(
            result.is_err() && std::fs::read(&other).expect("other") == SENTINEL,
            "the open after a replace refuses a hard link: {result:?}"
        );
    }
}

/// **A FAILED APPEND LETS THE HELD `acked` DESCRIPTOR GO** (D05): after an append whose
/// write failed, a write made and failed (a read-only descriptor in the slot: `EBADF`) or
/// one refused before it was made (the test's refusal, as the sim's write errors are), no
/// descriptor is held, so the next append opens and checks the file the name holds then: a
/// file put at the name after the failure takes the next ack. A descriptor kept across the
/// failure fails again (the read-only one) or writes the ack into the file the name no
/// longer holds (the refused one), and opens nothing.
#[test]
fn a_failed_append_lets_the_held_acked_descriptor_go() {
    let scratch = crate::test_scratch_dir("acked");
    let root = std::fs::canonicalize(scratch.path()).expect("canonical state scratch");
    let mut seen = Vec::new();
    for (fails, dir) in [
        ("its write fails", "fails"),
        ("its write is refused", "refused"),
    ] {
        let st = StateDir::open(root.join(dir)).expect("state");
        st.append_self_acked(None, &[1])
            .expect("the first append, which opens the log");
        let failed = if dir == "fails" {
            let read_only = std::fs::File::open(st.path("acked")).expect("acked, read-only");
            *st.acked.lock().unwrap_or_else(|e| e.into_inner()) = Some(read_only);
            st.append_self_acked(None, &[2])
        } else {
            st.refuse_writes_of(Some("acked"));
            let failed = st.append_self_acked(None, &[2]);
            st.refuse_writes_of(None);
            failed
        };
        let swap = root.join(format!("{dir}.swap"));
        std::fs::write(&swap, "7,").expect("a file for the name");
        std::fs::rename(&swap, st.path("acked")).expect("put it at the name");
        let before = st.ops();
        let next = st.append_self_acked(None, &[3]);
        seen.push((
            fails,
            failed.is_err(),
            next.is_ok(),
            (st.ops() - before).opens,
            std::fs::read_to_string(st.path("acked")).expect("acked"),
        ));
    }
    assert_eq!(
        seen,
        [
            ("its write fails", true, true, 1, "7,3,".to_string()),
            ("its write is refused", true, true, 1, "7,3,".to_string()),
        ],
        "(the failure, the append failed, the next landed, the next's opens, acked)"
    );
}

#[test]
fn a_re_base_drops_proven_old_references_and_keeps_uncertain_or_unrelated_mail() {
    use astream_broker::LogIdentity;
    let (a, b) = (
        LogIdentity {
            id: [0xaa; 16],
            generation: 0,
        },
        LogIdentity {
            id: [0xbb; 16],
            generation: 0,
        },
    );
    let dir = scratch("inflight-rebase");
    let st = StateDir::open(&dir).unwrap();
    let face = "/f/f/in/n-a/s-b/n-a/expired";
    let entry = |seq: u64, log: Option<LogIdentity>| InFlight {
        pin: PublishPin {
            seq,
            via_key: false,
            identity: None,
            original: None,
            original_off: None,
            owner: None,
            at: None,
        },
        subject: face.into(),
        body: format!("v=1 t=1 re={seq}").into_bytes(),
        log,
        at: None,
    };
    st.set_in_flight(&entry(1, Some(a))).unwrap();
    st.set_in_flight(&entry(2, None)).unwrap();
    st.set_in_flight(&entry(3, Some(b))).unwrap();
    let note = InFlight {
        body: b"v=1 t=1 text=still-owed".to_vec(),
        ..entry(4, Some(a))
    };
    st.set_in_flight(&note).unwrap();
    assert_eq!(
        st.forget_log_offsets(100, Some(b), None, None).unwrap(),
        vec![(1, face.to_string())]
    );
    let kept = vec![(2, entry(2, None)), (3, entry(3, Some(b))), (4, note)];
    assert_eq!(st.in_flight().unwrap(), kept);
    assert!(
        st.forget_log_offsets(100, Some(b), None, None)
            .unwrap()
            .is_empty(),
        "idempotent"
    );
    assert!(
        st.forget_log_offsets(7, None, None, None)
            .unwrap()
            .is_empty(),
        "no identity proves no mismatch"
    );
    assert_eq!(st.in_flight().unwrap(), kept, "uncertainty is retained");
    st.set_seen_off("s-b", 44).unwrap();
    st.write("inflight/5", "damaged").unwrap();
    assert_eq!(
        StateDir::open(&dir)
            .unwrap()
            .forget_log_offsets(7, Some(b), None, None)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert!(dir.join("inflight/5").exists(), "damage is kept for repair");
    assert_eq!(st.seen_off("s-b"), Some(44), "preflight changed nothing");
    std::fs::remove_file(dir.join("inflight/5")).unwrap();
    let journal = dir.join(journal::FILE);
    let mut damaged = std::fs::read(&journal).unwrap();
    damaged[20] ^= 0x01;
    std::fs::write(&journal, &damaged).unwrap();
    assert_eq!(
        StateDir::open(&dir)
            .unwrap()
            .forget_log_offsets(7, Some(b), None, None)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData,
        "a damaged journal"
    );
    assert_eq!(std::fs::read(&journal).unwrap(), damaged, "kept for repair");
    assert_eq!(st.seen_off("s-b"), Some(44), "preflight changed nothing");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_moved_generation_clamps_the_offsets_it_continues_and_forgets_the_rest() {
    use astream_broker::LogIdentity;
    let old = LogIdentity {
        id: [0xaa; 16],
        generation: 0,
    };
    let new = LogIdentity {
        generation: 1,
        ..old
    };
    let pin = |original_off: Option<u64>| PublishPin {
        seq: 1,
        via_key: true,
        identity: original_off.map(|_| PublishIdentity {
            subject: "/f/f/in/n-a/s-b/n-c/ask".into(),
            digest: "a".repeat(64),
        }),
        original: None,
        original_off,
        owner: None,
        at: None,
    };
    let in_flight = |seq: u64, re: u64| InFlight {
        pin: PublishPin { seq, ..pin(None) },
        subject: "/f/f/in/n-a/s-b/n-a/answer".into(),
        body: format!("v=1 t=1 re={re}").into_bytes(),
        log: Some(old),
        at: None,
    };
    let deadline = |off| Deadline {
        off,
        sid: "s-x".into(),
        at: 9_000,
        dl: 60,
        to: None,
    };
    let asked = |off| Asked {
        off,
        to: "n-b".into(),
    };
    let cursors = |pairs: &[(&str, u64)]| -> BTreeMap<String, TopicCursor> {
        pairs
            .iter()
            .map(|(t, next)| ((*t).to_string(), TopicCursor { next: *next }))
            .collect()
    };
    let floors = |pairs: &[(&str, u64)]| -> BTreeMap<String, u64> {
        pairs
            .iter()
            .map(|(t, off)| ((*t).to_string(), *off))
            .collect()
    };
    let fill = |st: &StateDir| {
        st.set_seen_off("s-low", 3).unwrap();
        st.set_seen_off("s-high", 9).unwrap();
        st.set_topics("s-x", &cursors(&[("a", 2), ("b", 8)]))
            .unwrap();
        st.set_topic_floors("s-x", &floors(&[("a", 1), ("b", 7)]))
            .unwrap();
        st.set_topic_floors("s-y", &floors(&[("c", 6)])).unwrap();
        st.set_halt_acked(2).unwrap();
        st.set_inbox_owed("n-a", 4).unwrap();
        st.set_deadlines_as_an_older_build(&[deadline(3), deadline(6)])
            .unwrap();
        st.set_asked_as_an_older_build(&[asked(3), asked(6)])
            .unwrap();
        st.set_key_pin("s-x", "low", &pin(Some(1))).unwrap();
        st.set_key_pin("s-x", "high", &pin(Some(6))).unwrap();
        st.set_in_flight(&in_flight(1, 2)).unwrap();
        st.set_in_flight(&in_flight(2, 7)).unwrap();
        st.set_self_acked(&[1, 6].into_iter().collect(), 0, 8)
            .unwrap();
    };
    let answer = |seq: u64| (seq, "/f/f/in/n-a/s-b/n-a/answer".to_string());

    let dir = scratch("clamp");
    let st = StateDir::open(&dir).unwrap();
    fill(&st);
    assert_eq!(
        st.forget_log_offsets(12, Some(new), Some(5), None).unwrap(),
        vec![answer(2)],
        "only the in-flight reply to a record the loss took goes"
    );
    let clamped = |st: &StateDir| {
        assert_eq!(st.seen_off("s-low"), Some(3), "below the fork: kept");
        assert_eq!(st.seen_off("s-high"), Some(4), "at the fork");
        assert_eq!(st.topics("s-x"), cursors(&[("a", 2), ("b", 5)]));
        assert_eq!(st.topic_floors("s-x").unwrap(), floors(&[("a", 1)]));
        assert!(st.topic_floors("s-y").unwrap().is_empty());
        assert_eq!(st.halt_acked(), None, "cleared, clamped or not");
        assert_eq!(
            st.inbox_owed("n-a"),
            Some(4),
            "the commit owed is below the fork"
        );
        assert_eq!(st.deadlines(), [deadline(3)]);
        assert_eq!(st.asked(), [asked(3)]);
        assert_eq!(
            st.key_pin("s-x", "low").unwrap().unwrap().original_off,
            Some(1)
        );
        assert_eq!(
            st.key_pin("s-x", "high").unwrap().unwrap().original_off,
            None
        );
        assert_eq!(
            st.in_flight().unwrap(),
            vec![(
                1,
                InFlight {
                    log: Some(new),
                    ..in_flight(1, 2)
                }
            )],
            "the reply below the fork is kept, RE-WITNESSED onto the new generation: the \
             attach replays a reference only on the log its witness names"
        );
        let acked = st.self_acked_log();
        assert_eq!(
            (acked.offsets, acked.floor),
            (std::collections::BTreeSet::new(), Some(12)),
            "the window re-bases at the head: empty, its floor the head"
        );
    };
    clamped(&st);
    assert!(
        st.forget_log_offsets(12, Some(new), Some(5), None)
            .unwrap()
            .is_empty(),
        "idempotent"
    );
    clamped(&st);
    st.set_topics("s-z", &cursors(&[("d", 30)])).unwrap();
    st.forget_log_offsets(12, Some(new), Some(40), None)
        .unwrap();
    assert_eq!(st.topics("s-z"), cursors(&[("d", 12)]));

    let twin = scratch("clamp-ctl");
    let st = StateDir::open(&twin).unwrap();
    fill(&st);
    assert_eq!(
        st.forget_log_offsets(12, Some(new), None, None).unwrap(),
        vec![answer(1), answer(2)]
    );
    assert_eq!((st.seen_off("s-low"), st.seen_off("s-high")), (None, None));
    assert_eq!(st.topics("s-x"), cursors(&[("a", 12), ("b", 12)]));
    assert!(st.topic_floors("s-x").unwrap().is_empty());
    assert_eq!(st.halt_acked(), None);
    assert_eq!(st.inbox_owed("n-a"), None);
    assert!(st.deadlines().is_empty() && st.asked().is_empty());
    assert_eq!(
        st.key_pin("s-x", "low").unwrap().unwrap().original_off,
        None
    );
    assert!(st.in_flight().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&twin);
}

#[test]
fn an_in_flight_reply_is_kept_only_below_the_fork_of_the_log_it_was_witnessed_on() {
    let log = |generation| astream_broker::LogIdentity {
        id: [0x3c; 16],
        generation,
    };
    let lineage =
        astream_broker::Lineage::from_steps(&[(1, 41), (2, 138)]).expect("a lineage astream reads");
    let entry = |seq: u64, re: u64, generation: u64| InFlight {
        pin: PublishPin {
            seq,
            via_key: false,
            identity: None,
            original: None,
            original_off: None,
            owner: None,
            at: None,
        },
        subject: "/f/f/in/n-a/s-b/n-a/ack".into(),
        body: format!("v=1 t=1 re={re}").into_bytes(),
        log: Some(log(generation)),
        at: None,
    };
    let fill = |st: &StateDir| {
        st.set_in_flight(&entry(1, 40, 0)).unwrap();
        st.set_in_flight(&entry(2, 41, 0)).unwrap();
        st.set_in_flight(&entry(3, 137, 1)).unwrap();
        st.set_in_flight(&entry(4, 138, 1)).unwrap();
    };
    let ack = |seq: u64| (seq, "/f/f/in/n-a/s-b/n-a/ack".to_string());
    let dir = scratch("own-fork");
    let st = StateDir::open(&dir).unwrap();
    fill(&st);
    let keep_below = LogWitness {
        high: 150,
        id: Some(log(1)),
    }
    .continues(Some(log(2)), Some(&lineage));
    assert_eq!(keep_below, Some(138), "the newest move's fork");
    assert_eq!(
        st.forget_log_offsets(150, Some(log(2)), keep_below, Some(&lineage))
            .unwrap(),
        vec![ack(2), ack(4)],
        "generation 0's reply at 41 and generation 1's at 138 go"
    );
    let rewitnessed = |seq, re, generation| {
        (
            seq,
            InFlight {
                log: Some(log(2)),
                ..entry(seq, re, generation)
            },
        )
    };
    assert_eq!(
        st.in_flight().unwrap(),
        vec![rewitnessed(1, 40, 0), rewitnessed(3, 137, 1)],
        "each kept below its own fork, re-witnessed onto generation 2"
    );
    let twin = scratch("own-fork-none");
    let st = StateDir::open(&twin).unwrap();
    fill(&st);
    assert_eq!(
        st.forget_log_offsets(150, Some(log(2)), keep_below, None)
            .unwrap(),
        vec![ack(4)]
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&twin);
}

#[test]
fn a_key_pins_its_sequence_first_wins_and_the_4097th_evicts_the_oldest() {
    let dir = scratch("keys");
    let st = StateDir::open(&dir).expect("open");
    assert_eq!(st.key_seq("s-a", "k-1"), None);
    assert_eq!(st.set_key_seq("s-a", "k-1", 41).expect("reserve"), 41);
    assert_eq!(
        StateDir::open(&dir).expect("reopen").key_seq("s-a", "k-1"),
        Some(41),
        "the reservation survives a reopen"
    );
    assert_eq!(st.set_key_seq("s-a", "k-1", 99).expect("re-reserve"), 41);
    assert_eq!(st.key_seq("s-a", "k-1"), Some(41));
    assert_eq!(st.key_seq("s-b", "k-1"), None);
    assert_eq!(st.set_key_seq("s-b", "k-1", 7).expect("reserve"), 7);
    assert_eq!(st.key_seq("s-a", "k-1"), Some(41));

    let full: Vec<String> = (0..KEYS_KEEP).map(|n| format!("k-{n} {n}")).collect();
    st.write("keys/s-c", &full.join("\n"))
        .expect("seed a full table");
    assert_eq!(st.key_seq("s-c", "k-0"), Some(0));
    assert_eq!(
        st.key_seq("s-c", &format!("k-{}", KEYS_KEEP - 1)),
        Some(KEYS_KEEP as u64 - 1)
    );
    assert_eq!(
        st.set_key_seq("s-c", "k-newest", 1_000_000)
            .expect("the 4097th"),
        1_000_000
    );
    assert_eq!(st.key_seq("s-c", "k-0"), None, "the oldest key is evicted");
    assert_eq!(st.key_seq("s-c", "k-1"), Some(1), "the next-oldest stands");
    assert_eq!(st.key_seq("s-c", "k-newest"), Some(1_000_000));
    assert_eq!(st.key_lines("s-c").len(), KEYS_KEEP, "the table is capped");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deadlines_round_trip_are_bounded_and_a_damaged_line_reads_as_nothing() {
    let dir = scratch("deadlines");
    let st = StateDir::open(&dir).expect("open");
    assert!(st.deadlines().is_empty());
    let one = Deadline {
        off: 90_312,
        sid: "s-abcdef0123456789".to_string(),
        at: 1_700_000_240_123,
        dl: 240_000,
        to: Some("n-0123456789abcdef".to_string()),
    };
    st.set_deadlines_as_an_older_build(std::slice::from_ref(&one))
        .expect("write");
    assert_eq!(
        StateDir::open(&dir).expect("reopen").deadlines(),
        vec![one.clone()]
    );
    st.set_deadlines_as_an_older_build(&[]).expect("clear");
    assert!(st.deadlines().is_empty());
    for damaged in [
        "",
        "off=1 sid=s-a at=2",          // no dl
        "off=1 sid=s-a dl=3",          // no at
        "sid=s-a at=2 dl=3",           // no offset
        "off=1 at=2 dl=3",             // no sid
        "off=1 sid= at=2 dl=3",        // an empty sid
        "off=x sid=s-a at=2 dl=3",     // an offset that is not one
        "off=1 sid=s-a at=2 dl=3 to=", // an empty recipient
    ] {
        assert_eq!(Deadline::parse(damaged), None, "{damaged:?}");
    }
    assert_eq!(Deadline::parse(&one.render()), Some(one.clone()));
    assert_eq!(
        Deadline::parse("off=90312 sid=s-abcdef0123456789 at=1700000240123 dl=240000"),
        Some(Deadline { to: None, ..one })
    );
    let many: Vec<Deadline> = (0..DEADLINES_KEEP as u64 + 3)
        .map(|n| Deadline {
            off: n,
            sid: "s-a".to_string(),
            at: n,
            dl: 1,
            to: None,
        })
        .collect();
    st.set_deadlines_as_an_older_build(&many)
        .expect("write many");
    let kept = st.deadlines();
    assert_eq!(kept.len(), DEADLINES_KEEP);
    assert_eq!(kept[0].off, 3, "the three oldest are gone");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_asked_table_round_trips_and_is_bounded() {
    let dir = scratch("asked");
    let st = StateDir::open(&dir).expect("open");
    assert!(st.asked().is_empty());
    let two = vec![
        Asked {
            off: 7,
            to: "n-0123456789abcdef".to_string(),
        },
        Asked {
            off: 9,
            to: "h-andrew".to_string(),
        },
    ];
    st.set_asked_as_an_older_build(&two).expect("write");
    assert_eq!(StateDir::open(&dir).expect("reopen").asked(), two);
    for damaged in ["", "7", "x n-a", "7 n-a extra"] {
        assert_eq!(Asked::parse(damaged), None, "{damaged:?}");
    }
    let many: Vec<Asked> = (0..ASKED_KEEP as u64 + 2)
        .map(|off| Asked {
            off,
            to: "n-a".to_string(),
        })
        .collect();
    st.set_asked_as_an_older_build(&many).expect("write many");
    let kept = st.asked();
    assert_eq!(kept.len(), ASKED_KEEP);
    assert_eq!(kept[0].off, 2, "the two oldest are gone");
    let _ = std::fs::remove_dir_all(&dir);
}

fn ask_deadline(off: u64, sid: &str) -> Deadline {
    Deadline {
        off,
        sid: sid.to_string(),
        at: 1_700_000_000_000 + off,
        dl: 60_000,
        to: Some("n-b".to_string()),
    }
}

fn ask_recipient(off: u64) -> Asked {
    Asked {
        off,
        to: "n-b".to_string(),
    }
}

/// **AN ASK COSTS THE STATE DIR ONE APPEND** (C04): its recipient and its deadline noted
/// together are one write of the asks journal, and nothing is replaced; a settle is one more.
/// What a build before the journal did instead: two whole tables replaced an ask.
#[test]
fn an_ask_costs_one_append_and_its_settle_one_more() {
    let dir = scratch("asks-cost");
    let st = StateDir::open(&dir).expect("open");
    for off in [5, 9] {
        let before = st.ops();
        st.note_asks([
            asks::Frame::Asked(ask_recipient(off)),
            asks::Frame::Deadline(ask_deadline(off, "s-a")),
        ])
        .expect("noted");
        st.write_asks().expect("written");
        let spent = st.ops() - before;
        assert_eq!(
            (spent.appends, spent.replaces, spent.renames, spent.fsyncs),
            (1, 0, 0, 0),
            "the ask at {off}"
        );
    }
    let before = st.ops();
    st.note_asks([asks::Frame::DeadlineGone {
        off: 5,
        sid: "s-a".to_string(),
    }])
    .expect("noted");
    st.write_asks().expect("written");
    assert_eq!((st.ops() - before).appends, 1, "the settle");
    let before = st.ops();
    st.write_asks().expect("nothing owed");
    assert_eq!(
        st.ops() - before,
        FileOps::default(),
        "nothing owed, nothing written"
    );
    let reopened = StateDir::open(&dir).expect("reopen");
    assert_eq!(reopened.asked(), [ask_recipient(5), ask_recipient(9)]);
    assert_eq!(reopened.deadlines(), [ask_deadline(9, "s-a")]);
    assert!(!dir.join(asks::DEADLINES).exists() && !dir.join(asks::ASKED).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A SETTLED DEADLINE IS REMEMBERED ACROSS A REOPEN** (C04): the settle replays at the
/// next open, after a rewrite from the index too, and after a stop's hand-off to the tables
/// a build before the journal reads, so the next bridge's retry of the ask arms nothing
/// (`Bridge::note_deadline`). CONTROL: another asker's is not.
#[test]
fn a_settled_deadline_is_remembered_across_a_reopen_a_rewrite_and_a_hand_off() {
    let dir = scratch("asks-settled");
    let st = StateDir::open(&dir).expect("open");
    st.note_asks([
        asks::Frame::Asked(ask_recipient(5)),
        asks::Frame::Deadline(ask_deadline(5, "s-a")),
    ])
    .expect("noted");
    st.write_asks().expect("written");
    st.note_asks([asks::Frame::DeadlineGone {
        off: 5,
        sid: "s-a".to_string(),
    }])
    .expect("noted");
    st.write_asks().expect("written");
    let reopened = StateDir::open(&dir).expect("reopen");
    assert!(reopened.deadline_settled(5, "s-a"), "the settle replays");
    assert!(!reopened.deadline_settled(5, "s-b"), "only its asker's");
    assert!(reopened.deadlines().is_empty());

    reopened.refuse_writes_of(Some(asks::FILE));
    reopened
        .note_asks([asks::Frame::Asked(ask_recipient(6))])
        .expect("noted");
    assert!(reopened.write_asks().is_err(), "refused: rewritten next");
    reopened.refuse_writes_of(None);
    let before = reopened.ops();
    reopened.write_asks().expect("the rewrite");
    assert_eq!(
        (reopened.ops() - before).replaces,
        1,
        "a rewrite, from the index"
    );
    assert!(StateDir::open(&dir)
        .expect("reopen")
        .deadline_settled(5, "s-a"));
    assert_eq!(
        StateDir::open(&dir).expect("reopen").asked(),
        [ask_recipient(5), ask_recipient(6)],
        "the refused record, rewritten"
    );

    reopened.hand_asks_to_files().expect("the hand-off");
    let table = std::fs::read_to_string(dir.join(asks::DEADLINES)).expect("handed back");
    assert_eq!(asks::legacy_mark(&table), Some(1));
    assert!(table.lines().all(|line| Deadline::parse(line).is_none()));
    assert!(StateDir::open(&dir)
        .expect("reopen")
        .deadline_settled(5, "s-a"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Settle deadline `off`, then more than the index remembers, so that settle ages out.
fn settle_past_the_memory(st: &StateDir, off: u64) {
    let settle = |off| asks::Frame::DeadlineGone {
        off,
        sid: "s-a".to_string(),
    };
    st.note_asks([settle(off)]).expect("noted");
    st.note_asks((0..=DEADLINES_KEEP as u64).map(|n| settle(off + 1 + n)))
        .expect("noted");
    st.write_asks().expect("written");
    assert!(!st.deadline_settled(off, "s-a"), "aged out");
}

/// **AN UNMARKED TABLE IS TAKEN IN ONCE** (C04 review): an older build's seamless hand-off
/// leaves its table with no hand-off named, and no stop of this build replaces it. The
/// deadline it holds is settled here, and the settle ages out of what the index remembers;
/// a kill and the next open take nothing in, so the deadline is not armed again for a sweep
/// to expire an answered ask. CONTROL: with the journal gone, what took it is gone too, so
/// the table's row is the only word there is, and it stands.
#[test]
fn an_unmarked_table_is_taken_in_once_and_a_settle_that_aged_out_stays_settled() {
    let dir = scratch("asks-taken-once");
    let st = StateDir::open(&dir).expect("the older build's dir");
    st.set_deadlines_as_an_older_build(&[ask_deadline(3, "s-a")])
        .expect("its table");
    drop(st);
    let st = StateDir::open(&dir).expect("this build");
    assert_eq!(st.asks_tables().1, [ask_deadline(3, "s-a")], "taken in");
    settle_past_the_memory(&st, 3);
    drop(st);
    let reopened = StateDir::open(&dir).expect("after a kill");
    assert!(reopened.asks_tables().1.is_empty(), "armed again nowhere");
    assert!(StateDir::peek(&dir).deadlines().is_empty(), "nor by a peek");
    assert!(
        Deadline::parse(
            std::fs::read_to_string(dir.join(asks::DEADLINES))
                .expect("the table, left where it was")
                .trim()
        )
        .is_some(),
        "what keeps it out is the journal's record, not the file"
    );
    drop(reopened);
    std::fs::remove_file(dir.join(asks::FILE)).expect("CONTROL: the journal gone");
    assert_eq!(
        StateDir::open(&dir).expect("reopen").asks_tables().1,
        [ask_deadline(3, "s-a")],
        "CONTROL: the table is the only word"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A TABLE THAT NAMES THIS JOURNAL'S HAND-OFF IS NOT READ, HOWEVER OLD THE SETTLE** (C04
/// review): a stop hands deadline 3 back and records the hand-off (`Frame::Handed`). The next
/// run settles 3 and more than the index remembers, and is killed, so the table still holds
/// the row and the settle has aged out. The next open reads the table's line and arms
/// nothing. Without the record the journal would name no hand-off, and the table's row would
/// come back.
#[test]
fn a_table_of_this_journals_hand_off_arms_nothing_a_later_run_settled() {
    let dir = scratch("asks-handed-record");
    let st = StateDir::open(&dir).expect("open");
    st.note_asks([asks::Frame::Deadline(ask_deadline(3, "s-a"))])
        .expect("noted");
    st.write_asks().expect("written");
    st.hand_asks_to_files().expect("the hand-off");
    assert_eq!(
        asks::read(&std::fs::read(dir.join(asks::FILE)).expect("the journal"))
            .index
            .handed,
        Some(1),
        "the hand-off is recorded"
    );
    drop(st);
    let next = StateDir::open(&dir).expect("the next run");
    settle_past_the_memory(&next, 3);
    drop(next);
    assert!(
        StateDir::open(&dir)
            .expect("after a kill")
            .asks_tables()
            .1
            .is_empty(),
        "the row its own hand-off left in the table is not armed again"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **AN OLDER BUILD THAT MET A MOVE OF THE LOG BETWEEN TWO RUNS OF THIS ONE HAS THE LAST WORD**
/// (C04 review): this build stops with asks 40 and 50 to `n-old`, deadline 50 standing and 40
/// settled. An older build runs, forgets everything on the move, and s-a's next ask lands at
/// 40 on the new log, to `n-new`. The next open holds that build's rows alone: 40 expires
/// (no remembered settle keeps its deadline out), no sweep of old 50 runs on the new log, and
/// a reply from `n-new` to 40 is no stray. Once taken, that table is not read again. CONTROL:
/// the same build holding nothing leaves empty tables, which say the same.
#[test]
fn an_older_builds_move_of_the_log_between_runs_has_the_last_word() {
    let to = |off, node: &str| Asked {
        off,
        to: node.to_string(),
    };
    let at_old = |off| Deadline {
        to: Some("n-old".to_string()),
        ..ask_deadline(off, "s-a")
    };
    for leaves_nothing in [false, true] {
        let dir = scratch("asks-older-move");
        let st = StateDir::open(&dir).expect("open");
        st.note_asks([
            asks::Frame::Asked(to(40, "n-old")),
            asks::Frame::Deadline(at_old(40)),
            asks::Frame::Asked(to(50, "n-old")),
            asks::Frame::Deadline(at_old(50)),
            asks::Frame::DeadlineGone {
                off: 40,
                sid: "s-a".to_string(),
            },
        ])
        .expect("noted");
        st.write_asks().expect("written");
        st.hand_asks_to_files().expect("the stop");

        let on_the_new_log = Deadline {
            to: Some("n-new".to_string()),
            ..ask_deadline(40, "s-a")
        };
        let (asked, deadlines) = if leaves_nothing {
            (Vec::new(), Vec::new())
        } else {
            (vec![to(40, "n-new")], vec![on_the_new_log])
        };
        st.set_asked_as_an_older_build(&asked).expect("its table");
        st.set_deadlines_as_an_older_build(&deadlines)
            .expect("its table");
        drop(st);

        let back = StateDir::open(&dir).expect("this build again");
        assert_eq!(back.asks_tables(), (asked.clone(), deadlines.clone()));
        assert!(
            !back.deadline_settled(40, "s-a"),
            "no settle of the old log"
        );
        assert_eq!(
            StateDir::peek(&dir).deadlines(),
            deadlines,
            "a peek reads so"
        );
        back.write_asks().expect("what it took, written");
        drop(back);
        let again = StateDir::open(&dir).expect("reopen");
        assert_eq!(again.asks_tables(), (asked, deadlines));
        let before = again.ops();
        again.write_asks().expect("nothing owed");
        assert_eq!(
            (again.ops() - before).replaces,
            0,
            "taken once, so nothing is rewritten"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// **DAMAGE AHEAD OF A HAND-OFF'S RECORD TAKES THE ROW BACK FROM THE TABLE** (C04 review): two
/// deadlines are written and a stop hands them off, so the tables name the hand-off the
/// journal records. A power cut after a hand-off stop, which flushes nothing, damages the
/// first deadline's record. The open and the sweep read that deadline back from the table,
/// and the rewrite holds it. CONTROL: undamaged, the same open reads the same rows, taking
/// nothing in.
#[test]
fn damage_ahead_of_a_hand_off_takes_the_row_back_from_the_table() {
    for sweep in [false, true] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("asks-damage-handed")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(in_boot(1));
        st.take_incarnation(0).expect("the last attach, in boot 1");
        for off in [3, 4] {
            st.note_asks([asks::Frame::Deadline(ask_deadline(off, "s-a"))])
                .expect("noted");
            st.write_asks().expect("written");
        }
        st.hand_asks_to_files().expect("the hand-off");
        drop(st);

        let control = StateDir::open(root).expect("CONTROL");
        assert_eq!(
            control.asks_tables().1,
            [ask_deadline(3, "s-a"), ask_deadline(4, "s-a")]
        );
        let before = control.ops();
        control.write_asks().expect("nothing owed");
        assert_eq!(
            (control.ops() - before).replaces,
            0,
            "CONTROL: nothing taken in"
        );
        drop(control);

        let path = root.join(asks::FILE);
        let mut bytes = std::fs::read(&path).expect("the journal");
        bytes[20] ^= 0x01;
        std::fs::write(&path, &bytes).expect("damage");
        assert!(matches!(
            asks::read(&bytes).end,
            journal::End::Fault { .. } | journal::End::Torn
        ));
        let rows = [ask_deadline(3, "s-a"), ask_deadline(4, "s-a")];
        let st = StateDir::open(root).expect("the next bridge");
        if sweep {
            st.set_inc_seam(in_boot(2));
            st.set_aside_torn().expect("the sweep");
        }
        assert_eq!(st.asks_tables().1, rows, "the lost row back");
        st.write_asks().expect("the rewrite");
        assert_eq!(
            asks::read(&std::fs::read(&path).expect("rewritten"))
                .index
                .deadline_rows(),
            rows,
            "the rewrite holds it"
        );
    }
}

/// **THE ASKS JOURNAL IS REWRITTEN FROM ITS INDEX PAST ITS BOUND AND KEEPS WHAT IS LIVE**
/// (C04 review): nothing below the bound is rewritten, a rewrite holds only what the index
/// does, so a journal of settled asks shrinks, the file never runs past the bound by more
/// than a few records, and it reopens to the same index.
#[test]
fn the_asks_journal_is_rewritten_past_its_bound_and_keeps_what_is_live() {
    let dir = scratch("asks-bound");
    let st = StateDir::open(&dir).expect("open");
    st.note_asks([
        asks::Frame::Asked(ask_recipient(1)),
        asks::Frame::Deadline(ask_deadline(1, "s-live")),
    ])
    .expect("a live deadline");
    st.write_asks().expect("written");
    let path = dir.join(asks::FILE);
    let size = || std::fs::metadata(&path).expect("the journal").len();
    let (mut rewrites, mut bound, mut off) = (0, journal::COMPACT_AT, 1_000u64);
    while rewrites < 2 {
        off += 1;
        st.note_asks([
            asks::Frame::Asked(ask_recipient(off)),
            asks::Frame::Deadline(ask_deadline(off, "s-a")),
            asks::Frame::DeadlineGone {
                off,
                sid: "s-a".to_string(),
            },
        ])
        .expect("noted");
        let before = (size(), st.ops().replaces);
        assert!(
            before.0 < bound + 4096,
            "past its bound of {bound} by more than one write: {} bytes",
            before.0
        );
        st.write_asks().expect("written");
        if st.ops().replaces > before.1 {
            rewrites += 1;
            assert!(size() < before.0, "a rewrite holds only what is live");
            assert!(
                before.0 >= bound,
                "CONTROL: nothing is rewritten below the bound ({} bytes)",
                before.0
            );
            bound = journal::COMPACT_AT.max(2 * size());
        }
    }
    let held = st.asks_tables();
    assert!(held.1.contains(&ask_deadline(1, "s-live")), "what is live");
    assert_eq!(
        StateDir::open(&dir).expect("reopen").asks_tables(),
        held,
        "reopens to what the open held"
    );
    assert_eq!(held.0.len(), ASKED_KEEP.min(off as usize - 999));
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A CLEAN STOP FLUSHES THE ASKS JOURNAL, APPENDED OR REWRITTEN** (C04 review): each kind
/// of write leaves its file for the stop to flush, so a power cut after a clean stop loses no
/// ask. CONTROL: a run that wrote nothing to it flushes nothing of it.
#[test]
fn a_clean_stop_flushes_the_asks_journal_appended_or_rewritten() {
    let stop_flushes = |write: &dyn Fn(&StateDir)| {
        let dir = aterm_tempfile::Builder::new()
            .prefix("asks-stop")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let st = StateDir::open(dir.path()).expect("open");
        st.set_inc_seam(in_boot(1));
        st.begin_run().expect("the run");
        write(&st);
        st.end_run_clean().expect("the stop").files
    };
    let note = |st: &StateDir| {
        st.note_asks([asks::Frame::Asked(ask_recipient(3))])
            .expect("noted");
    };
    assert_eq!(stop_flushes(&|_| {}), 0, "CONTROL: nothing written");
    assert_eq!(
        stop_flushes(&|st| {
            note(st);
            let before = st.ops();
            st.write_asks().expect("an append");
            assert_eq!((st.ops() - before).replaces, 0, "appended");
        }),
        1,
        "after an append"
    );
    assert_eq!(
        stop_flushes(&|st| {
            note(st);
            st.refuse_writes_of(Some(asks::FILE));
            assert!(st.write_asks().is_err(), "refused");
            st.refuse_writes_of(None);
            let before = st.ops().replaces;
            st.write_asks().expect("the rewrite");
            assert_eq!(st.ops().replaces, before + 1, "rewritten");
        }),
        1,
        "after a rewrite"
    );
}

/// **A HAND-OFF READS ROW FOR ROW TO A BUILD BEFORE THE JOURNAL, AND WHAT THAT BUILD WRITES
/// IS TAKEN BACK** (C04): a stop writes each table whole, its rows as that build's parse
/// reads them; the next open of this build takes nothing in from tables that name its own
/// hand-off. An older build that ran after it (a rollback) rewrites a table with no
/// hand-off named, and that table is the next open's word on what it holds: what that build
/// settled stays settled, and what it armed stands. The tables are never unlinked.
#[test]
fn a_hand_off_reads_to_an_older_build_and_its_tables_are_taken_back() {
    let dir = scratch("asks-hand-off");
    let st = StateDir::open(&dir).expect("open");
    st.hand_asks_to_files().expect("nothing to hand off");
    assert!(
        !dir.join(asks::ASKED).exists() && !dir.join(asks::FILE).exists(),
        "a dir that never asked writes nothing"
    );
    st.note_asks([
        asks::Frame::Asked(ask_recipient(3)),
        asks::Frame::Deadline(ask_deadline(3, "s-a")),
        asks::Frame::Asked(ask_recipient(4)),
        asks::Frame::Deadline(ask_deadline(4, "s-a")),
        asks::Frame::DeadlineGone {
            off: 4,
            sid: "s-a".to_string(),
        },
    ])
    .expect("noted");
    st.write_asks().expect("written");
    st.hand_asks_to_files().expect("the hand-off");
    let rows = |name: &str| -> Vec<String> {
        std::fs::read_to_string(dir.join(name))
            .expect("a table")
            .lines()
            .skip(1)
            .map(str::to_string)
            .collect()
    };
    let older_deadlines: Vec<Deadline> = std::fs::read_to_string(dir.join(asks::DEADLINES))
        .expect("the table")
        .lines()
        .filter_map(Deadline::parse)
        .collect();
    assert_eq!(
        older_deadlines,
        [ask_deadline(3, "s-a")],
        "what an older build reads"
    );
    assert_eq!(rows(asks::ASKED), ["3 n-b", "4 n-b"]);
    let reopened = StateDir::open(&dir).expect("reopen");
    let before = reopened.ops();
    assert_eq!(reopened.asks_tables().1, [ask_deadline(3, "s-a")]);
    reopened.write_asks().expect("nothing owed");
    assert_eq!(
        (reopened.ops() - before).replaces,
        0,
        "its own hand-off takes nothing in, so nothing is rewritten"
    );
    drop(reopened);

    // A rollback: a build before the journal settles 3, asks 7 and 8, and arms 4 again
    // (its retry of a held post, which knew no settle) and 8.
    st.set_deadlines_as_an_older_build(&[ask_deadline(4, "s-a"), ask_deadline(8, "s-a")])
        .expect("its table");
    st.set_asked_as_an_older_build(&[
        ask_recipient(3),
        ask_recipient(4),
        ask_recipient(7),
        ask_recipient(8),
    ])
    .expect("its table");
    let back = StateDir::open(&dir).expect("this build again");
    assert_eq!(
        back.asks_tables(),
        (
            vec![
                ask_recipient(3),
                ask_recipient(4),
                ask_recipient(7),
                ask_recipient(8)
            ],
            vec![ask_deadline(4, "s-a"), ask_deadline(8, "s-a")]
        ),
        "its word: 3 it settled stays settled, and 4 it armed again stands, for the sweep to \
         read its answer back from the bus"
    );
    let before = back.ops();
    back.write_asks().expect("the rewrite");
    assert_eq!(
        (back.ops() - before).replaces,
        1,
        "what was taken in is written"
    );
    assert!(dir.join(asks::ASKED).exists() && dir.join(asks::DEADLINES).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE SWEEP OF A TORN ASKS JOURNAL (C04), after an unclean stop in an earlier boot: its
/// exact bytes are kept in the quarantine, and it is rewritten with every whole record,
/// those after the damage too. CONTROL: a journal whose last record is cut short stays.
#[test]
fn the_sweep_keeps_every_whole_record_after_damage_in_the_asks_journal() {
    for cut in [false, true] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("asks-salvage")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(in_boot(1));
        st.take_incarnation(0).expect("the last attach, in boot 1");
        for off in [3, 4] {
            st.note_asks([
                asks::Frame::Asked(ask_recipient(off)),
                asks::Frame::Deadline(ask_deadline(off, "s-a")),
            ])
            .expect("noted");
            st.write_asks().expect("written");
        }
        drop(st);
        let path = root.join(asks::FILE);
        let mut bytes = std::fs::read(&path).expect("the journal");
        if cut {
            bytes.truncate(bytes.len() - 3);
        } else {
            let first =
                journal::encode(0, &asks::Frame::Asked(ask_recipient(3))).expect("a record");
            bytes[first.len() + 20] ^= 0x01;
        }
        std::fs::write(&path, &bytes).expect("damage");
        let st = StateDir::open(root).expect("boot 2's bridge");
        st.set_inc_seam(in_boot(2));
        let moved = st.set_aside_torn().expect("the sweep");
        if cut {
            assert!(moved.is_empty(), "CONTROL: a cut tail stays");
            assert_eq!(std::fs::read(&path).expect("kept"), bytes);
            assert_eq!(st.asks_tables().1, [ask_deadline(3, "s-a")]);
            continue;
        }
        assert_eq!(
            moved.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(),
            [asks::FILE]
        );
        assert_eq!(
            std::fs::read(root.join("quarantine/journal.asks")).expect("kept"),
            bytes
        );
        assert_eq!(
            st.asks_tables(),
            (
                vec![ask_recipient(3), ask_recipient(4)],
                vec![ask_deadline(4, "s-a")]
            ),
            "every whole record, those after the damaged deadline too"
        );
        assert_eq!(
            StateDir::open(root).expect("reopen").deadlines(),
            [ask_deadline(4, "s-a")],
            "rewritten whole"
        );
    }
}

#[test]
fn the_pin_table_is_bounded_and_evicts_the_oldest_first_sight() {
    let dir = scratch("pins-bound");
    let st = StateDir::open(&dir).expect("open");
    let full: Vec<String> = (0..PINS_KEEP)
        .map(|n| format!("s-{n:016x} n-{:016x}", n % 3))
        .collect();
    st.write("pins", &full.join("\n"))
        .expect("seed a full table");
    assert_eq!(st.pins().len(), PINS_KEEP);
    for n in PINS_KEEP..PINS_KEEP + 16 {
        st.pin(&format!("s-{n:016x}"), &format!("n-{:016x}", n % 3))
            .expect("pin");
    }
    let pins = st.pins();
    assert_eq!(pins.len(), PINS_KEEP, "the table is capped");
    for n in 0..16 {
        assert!(!pins.contains_key(&format!("s-{n:016x}")), "{n} evicted");
    }
    for n in 16..PINS_KEEP + 16 {
        assert_eq!(
            pins.get(&format!("s-{n:016x}")),
            Some(&format!("n-{:016x}", n % 3)),
            "{n} kept, with its node"
        );
    }
    let survivor = format!("s-{:016x}", PINS_KEEP);
    assert_eq!(
        st.pin(&survivor, "n-rogue").expect("re-pin"),
        format!("n-{:016x}", PINS_KEEP % 3)
    );
    assert_eq!(st.pins().len(), PINS_KEEP);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_bridge_lock_has_one_holder_until_it_drops() {
    let dir = scratch("lock");
    let st = StateDir::open(&dir).expect("open");
    let held = st.lock_bridge().expect("the first take");
    std::fs::write(dir.join("pid"), "4242\n").expect("pid");
    let refused = st.lock_bridge().err().expect("a second take is refused");
    assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
    assert!(refused.to_string().contains("(pid 4242)"), "{refused}");
    drop(held);
    let _again = st.lock_bridge().expect("free once the holder let go");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_log_switch_refuses_a_dangling_offset_directory() {
    for blocked in ["seen", "topics"] {
        let dir = scratch("dangling-offsets");
        let st = StateDir::open(&dir).unwrap();
        let area = dir.join(blocked);
        if area.is_dir() {
            std::fs::remove_dir(&area).unwrap();
        }
        std::os::unix::fs::symlink(dir.join("missing"), &area).unwrap();
        assert!(
            st.forget_log_offsets(20, None, None, None).is_err(),
            "unreadable {blocked} was empty"
        );
        assert!(area.is_symlink(), "the unreadable directory entry stays");
        std::fs::remove_file(&area).unwrap();
        assert!(
            st.forget_log_offsets(20, None, None, None).is_ok(),
            "an absent directory is empty"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn the_log_witness_round_trips_and_names_another_log_by_identity_or_head() {
    use astream_broker::LogIdentity;
    let a = LogIdentity {
        id: [0xaa; 16],
        generation: 0,
    };
    let a_rewritten = LogIdentity { generation: 1, ..a };
    let b = LogIdentity {
        id: [0xbb; 16],
        generation: 0,
    };
    let dir = scratch("witness");
    let st = StateDir::open(&dir).expect("open");
    assert_eq!(st.log_witness(), None, "nothing seen yet");
    st.set_log_witness(&LogWitness {
        high: 500,
        id: None,
    })
    .expect("write");
    let w = st.log_witness().expect("read back");
    assert_eq!(
        w,
        LogWitness {
            high: 500,
            id: None
        }
    );
    assert!(w.replaced_by(499, None) && w.replaced_by(499, Some(a)));
    assert!(!w.replaced_by(500, None) && !w.replaced_by(10_000, Some(a)));
    let with_id = LogWitness {
        high: 500,
        id: Some(a),
    };
    st.set_log_witness(&with_id).expect("write");
    assert_eq!(st.log_witness(), Some(with_id));
    assert!(with_id.replaced_by(10_000, Some(b)));
    assert!(with_id.replaced_by(10_000, Some(a_rewritten)));
    assert!(!with_id.replaced_by(10_000, Some(a)));
    assert!(
        !with_id.replaced_by(10_000, None),
        "no identity says nothing"
    );
    assert!(
        with_id.replaced_by(499, Some(a)),
        "a head below is evidence too"
    );
    assert_eq!(
        LogWitness::parse("id=00ff high=7"),
        Some(LogWitness { high: 7, id: None })
    );
    assert_eq!(LogWitness::parse("high=+7"), None);
    assert_eq!(LogWitness::parse("id=00ff"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn standing_halts_round_trip_per_fleet_and_a_damaged_line_is_nothing() {
    let dir = scratch("halts");
    let st = StateDir::open(&dir).expect("open");
    assert!(st.standing_halts("f").is_empty());
    let halts = BTreeMap::from([
        (
            "h-a".to_string(),
            StandingHalt {
                reason: "main%20broken".to_string(),
                kept: false,
            },
        ),
        (
            "h-b".to_string(),
            StandingHalt {
                reason: "fleet-halt".to_string(),
                kept: true,
            },
        ),
    ]);
    st.set_standing_halts("f", &halts).expect("write");
    assert_eq!(st.standing_halts("f"), halts);
    assert!(st.standing_halts("g").is_empty(), "another fleet's halts");
    std::fs::write(
        dir.join("halts"),
        "f h-a r witnessed\nf h-b r maybe\nf n-node r kept\nf h-c\n",
    )
    .expect("write");
    assert_eq!(
        st.standing_halts("f").into_keys().collect::<Vec<_>>(),
        ["h-a"]
    );
    st.set_standing_halts("f", &BTreeMap::new()).expect("empty");
    assert!(st.standing_halts("f").is_empty());

    st.set_halt_acked(9).expect("ack");
    st.clear_halt_acked().expect("clear");
    assert_eq!(st.halt_acked(), None);
    st.clear_halt_acked().expect("clearing nothing is no error");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_corrupt_file_reads_as_unknown() {
    let dir = scratch("corrupt");
    let st = StateDir::open(&dir).expect("open");
    std::fs::write(dir.join("seq"), "   \n").expect("write");
    assert_eq!(st.sequence(), 0);
    std::fs::write(dir.join("inc"), "not-a-number").expect("write");
    assert_eq!(st.incarnation(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

fn boot_id(b: u8) -> [u8; 16] {
    [b; 16]
}

fn in_boot(b: u8) -> IncSeam {
    IncSeam {
        boot: Some(Some(boot_id(b))),
        ..IncSeam::default()
    }
}

fn inc_file_id(n: u64) -> Option<IncFile> {
    Some(IncFile {
        dev: 1,
        dir: 2,
        ino: n,
    })
}

#[test]
fn an_incarnation_lease_round_trips_and_anything_torn_is_no_lease() {
    for lease in [
        IncLease {
            used: 3,
            top: 66,
            boot: Some(boot_id(7)),
            file: Some(IncFile {
                dev: u64::MAX,
                dir: 0,
                ino: 12_345,
            }),
        },
        IncLease {
            used: 5,
            top: 5,
            boot: None,
            file: None,
        },
    ] {
        assert_eq!(IncLease::parse(&lease.render()), Some(lease), "{lease:?}");
    }
    let id = aterm_codec::hex::encode_lower(&boot_id(7));
    for torn in [
        String::new(),
        "used=3 top=66".to_string(),
        format!("used=3 boot={id} file=1:2:3"),
        format!("used=3 top=66 boot={id}"),
        format!("used=3 top=66 boot={} file=1:2:3", &id[..31]),
        format!("used=3 top=66 boot={} file=1:2:3", "0".repeat(32)),
        format!("used=67 top=66 boot={id} file=1:2:3"),
        format!("used=3 used=4 top=66 boot={id} file=1:2:3"),
        format!("used=3 top=66 boot={id} file=1:2:3 extra=1"),
        format!("used=x top=66 boot={id} file=1:2:3"),
        format!("used=3 top=66 boot={id} file=1:2"),
        format!("used=3 top=66 boot={id} file=1:2:3:4"),
        format!("used=3 top=66 boot={id} file=1::3"),
        format!("used=3 top=66 boot={id} file=1:2:3 file=-"),
    ] {
        assert_eq!(IncLease::parse(&torn), None, "{torn:?}");
    }
}

#[test]
fn the_incarnation_plan_trusts_only_this_boots_lease_and_counts_seq_and_the_bus() {
    let (b1, b2) = (Some(boot_id(1)), Some(boot_id(2)));
    let f = inc_file_id(1);
    let lease = |used, top, boot| {
        Some(IncLease {
            used,
            top,
            boot,
            file: f,
        })
    };
    let plan = |next, top, flush| Some(IncPlan { next, top, flush });
    let span = INC_LEASE;
    assert_eq!(
        plan_incarnation(66, f, lease(3, 66, b1), 3, b1, 0, span),
        plan(4, 66, false)
    );
    assert_eq!(
        plan_incarnation(66, f, lease(66, 66, b1), 66, b1, 0, span),
        plan(67, 66 + span, true)
    );
    assert_eq!(
        plan_incarnation(66, f, lease(3, 66, b1), 3, b2, 0, span),
        plan(67, 66 + span, true)
    );
    assert_eq!(
        plan_incarnation(130, f, lease(3, 66, b1), 3, b1, 0, span),
        plan(131, 130 + span, true)
    );
    for (file, named) in [(inc_file_id(2), f), (None, f), (f, None), (None, None)] {
        let other = Some(IncLease {
            used: 3,
            top: 66,
            boot: b1,
            file: named,
        });
        assert_eq!(
            plan_incarnation(66, file, other, 3, b1, 0, span),
            plan(67, 66 + span, true),
            "inc {file:?}, lease naming {named:?}"
        );
    }
    assert_eq!(
        plan_incarnation(9, f, None, 9, b1, 0, span),
        plan(10, 9 + span, true)
    );
    assert_eq!(
        plan_incarnation(66, f, lease(3, 66, b1), 66, b1, 0, span),
        plan(67, 66 + span, true)
    );
    assert_eq!(
        plan_incarnation(66, f, lease(3, 66, b1), 3, b1, 90, span),
        plan(91, 90 + span, true)
    );
    assert_eq!(
        plan_incarnation(66, f, lease(3, 66, None), 3, None, 0, span),
        plan(67, 67, true)
    );
    assert_eq!(
        plan_incarnation(66, f, lease(3, 66, b1), 3, None, 0, span),
        plan(67, 67, true),
        "a lease naming this boot, read off the boot volume where no boot is trusted, is not \
         trusted, and leases one incarnation"
    );
    assert_eq!(
        plan_incarnation(INC_MAX - 1, f, None, 0, b1, 0, span),
        plan(INC_MAX, INC_MAX, true)
    );
    assert_eq!(plan_incarnation(INC_MAX, f, None, 0, b1, 0, span), None);
    assert_eq!(plan_incarnation(0, None, None, 0, b1, INC_MAX, span), None);
    assert_eq!(
        plan_incarnation(0, None, None, u64::MAX >> 32, b1, 0, span),
        None
    );
}

#[test]
fn a_lease_of_attaches_costs_one_flush_and_a_new_boot_flushes_once() {
    let dir = scratch("inclease-cost");
    let st = StateDir::open(&dir).expect("open");
    st.set_inc_seam(in_boot(1));
    let take = |n: u64| -> Vec<u64> {
        (0..n)
            .map(|_| st.take_incarnation(0).expect("take"))
            .collect()
    };
    let before = st.ops();
    let mut taken = take(3);
    assert_eq!(
        (st.incarnation(), st.incarnation_top()),
        (3, INC_LEASE),
        "the doctor's answer is what was taken, not the top"
    );
    taken.extend(take(INC_LEASE - 3));
    let lease = st.ops() - before;
    assert_eq!(
        taken,
        (1..=INC_LEASE).collect::<Vec<_>>(),
        "one apart, from 1"
    );
    assert_eq!(
        (lease.fsyncs, lease.replaces),
        (2, INC_LEASE + 1),
        "one flushed `inc`, then one `inc-lease` replace per attach: {lease:?}"
    );
    let before = st.ops();
    assert_eq!(st.take_incarnation(0).expect("take"), INC_LEASE + 1);
    assert_eq!(
        (st.ops() - before).fsyncs,
        2,
        "the lease ran out: flushed again"
    );

    st.set_inc_seam(in_boot(2));
    let before = st.ops();
    assert_eq!(
        st.take_incarnation(0).expect("take"),
        2 * INC_LEASE + 1,
        "above the whole lease"
    );
    assert_eq!((st.ops() - before).fsyncs, 2);
    let before = st.ops();
    assert_eq!(st.take_incarnation(0).expect("take"), 2 * INC_LEASE + 2);
    assert_eq!((st.ops() - before).fsyncs, 0, "and then trusted again");

    st.set_inc_seam(IncSeam {
        boot: Some(None),
        ..IncSeam::default()
    });
    for _ in 0..3 {
        let before = st.ops();
        let inc = st.take_incarnation(0).expect("take");
        assert_eq!((st.ops() - before).fsyncs, 2);
        assert_eq!(st.lease().map(|l| (l.used, l.top)), Some((inc, inc)));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// One take off the boot volume ([`takes_through_a_vanish`]): what it answered, the flushes it
/// made, and the lease it left (`used`, `top`, the boot it names).
type OffVolumeTake = (
    Result<u64, String>,
    u64,
    Option<(u64, u64, Option<[u8; 16]>)>,
);

/// Three attaches of a dir off the boot volume, all in boot 1, under the take's `mutant`:
/// after the second, the disk vanishes and comes back within the boot, holding `inc` as
/// flushed and the lease and `seq` the first attach wrote (written back, then nothing more).
fn takes_through_a_vanish(mutant: u8) -> Vec<OffVolumeTake> {
    let dir = scratch(&format!("inclease-vanish-{mutant}"));
    let st = StateDir::open(&dir).expect("open");
    st.set_inc_seam(IncSeam {
        boot: Some(Some(boot_id(1))),
        off_volume: true,
        mutant,
        ..IncSeam::default()
    });
    let read = |file: &str| std::fs::read(dir.join(file)).expect(file);
    let mut written_back = None;
    let mut takes = Vec::new();
    for attach in 1..=3 {
        if attach == 3 {
            let (lease, seq) = written_back.take().expect("the first attach's files");
            std::fs::write(dir.join("inc-lease"), lease).expect("the lease on the media");
            std::fs::write(dir.join("seq"), seq).expect("seq on the media");
        }
        let before = st.ops();
        let taken = st.take_incarnation(0).map_err(|e| e.to_string());
        let fsyncs = (st.ops() - before).fsyncs;
        if let Ok(inc) = taken {
            st.reserve_next_sequence(inc)
                .expect("the attach's first sequence");
        }
        let lease = st.lease().map(|l| (l.used, l.top, l.boot));
        takes.push((taken, fsyncs, lease));
        if attach == 1 {
            written_back = Some((read("inc-lease"), read("seq")));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    takes
}

/// Off the boot volume the page cache vouches for nothing on a disk that can vanish: a lease
/// that names this boot is not trusted ([`StateDir::trusted_boot`]), so each take flushes
/// `inc` and leases one incarnation, and the take after a vanish presents none twice (the
/// model's `Fence` at `OffVolume = 1`). Its negative control trusts by the boot the lease
/// names (mutant 5, the model's `Buggy = 5`).
#[test]
fn off_the_boot_volume_every_take_flushes_and_a_vanish_presents_no_incarnation_twice() {
    let b1 = Some(boot_id(1));
    let shipped = takes_through_a_vanish(0);
    assert_eq!(
        shipped,
        [
            (Ok(1), 2, Some((1, 1, b1))),
            (Ok(2), 2, Some((2, 2, b1))),
            (Ok(3), 2, Some((3, 3, b1))),
        ],
        "each take flushes `inc` (its file and its directory) and leaves a lease that names \
         this boot and leases nothing above it; after the vanish, the take is above both"
    );

    assert_eq!(
        takes_through_a_vanish(5),
        [
            (Ok(1), 2, Some((1, INC_LEASE, b1))),
            (Ok(2), 0, Some((2, INC_LEASE, b1))),
            (Ok(2), 0, Some((2, INC_LEASE, b1))),
        ],
        "the control trusts the lease by the boot it names: the second take flushes nothing, \
         and after the vanish the third presents the second's incarnation again"
    );
}

#[test]
fn the_doctor_never_answers_a_lost_leases_unused_top() {
    let dir = scratch("inclease-doctor");
    let st = StateDir::open(&dir).expect("open");
    st.set_inc_seam(in_boot(1));
    for want in 1..=3 {
        let inc = st.take_incarnation(0).expect("take");
        assert_eq!(inc, want);
        st.reserve_sequence_block(inc, 16).expect("a block");
    }
    assert_eq!(
        (st.incarnation(), st.incarnation_top()),
        (3, INC_LEASE),
        "the doctor answers the incarnation taken, not the lease's top"
    );

    std::fs::remove_file(dir.join("inc-lease")).expect("lose the lease");
    assert_eq!(
        st.incarnation(),
        3,
        "seq's incarnation, never the lost lease's unused top ({INC_LEASE})"
    );
    std::fs::write(dir.join("inc-lease"), "used=3 top=").expect("tear the lease");
    assert_eq!(st.incarnation(), 3, "a torn lease");
    std::fs::write(dir.join("seq"), (base(1) + 16).to_string()).expect("seq");
    assert_eq!(st.incarnation(), 1, "a rolled-back seq");
    std::fs::remove_file(dir.join("seq")).expect("lose seq");
    assert_eq!(st.incarnation(), 0, "nothing known");
    let seq = st
        .reserve_next_sequence(st.incarnation())
        .expect("a sequence");
    assert!(
        seq < will(1),
        "the doctor's sequence {seq:#x} is below every will this node registered"
    );

    std::fs::remove_file(dir.join("inc-lease")).expect("no lease");
    std::fs::write(dir.join("inc"), "9").expect("inc");
    std::fs::write(dir.join("seq"), (base(9) + 5).to_string()).expect("seq");
    assert_eq!(st.incarnation(), 9, "an older build's dir");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_copied_state_dir_flushes_its_own_inc_before_trusting_a_lease() {
    let (a, c) = (scratch("inclease-copy-a"), scratch("inclease-copy-c"));
    let original = StateDir::open(&a).expect("open the original");
    original.set_inc_seam(in_boot(1));
    for want in 1..=5 {
        assert_eq!(original.take_incarnation(0).expect("take"), want);
    }
    let copy = StateDir::open(&c).expect("open the copy");
    for file in ["inc", "inc-lease", "seq"] {
        if a.join(file).exists() {
            std::fs::copy(a.join(file), c.join(file)).expect("copy");
        }
    }
    copy.set_inc_seam(in_boot(1));
    let before = copy.ops();
    assert_eq!(
        copy.take_incarnation(0).expect("take"),
        INC_LEASE + 1,
        "above the copied lease's whole top"
    );
    assert_eq!(
        (copy.ops() - before).fsyncs,
        2,
        "the copy's own `inc` is flushed before its first incarnation is presented"
    );
    let before = copy.ops();
    assert_eq!(copy.take_incarnation(0).expect("take"), INC_LEASE + 2);
    assert_eq!(
        (copy.ops() - before).fsyncs,
        0,
        "and then its own lease is trusted"
    );
    let before = original.ops();
    assert_eq!(original.take_incarnation(0).expect("take"), 6);
    assert_eq!(
        (original.ops() - before).fsyncs,
        0,
        "the original keeps its lease"
    );
    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&c);
}

#[test]
fn a_state_dirs_mark_is_its_own_and_a_copy_has_another() {
    const NODE: &str = "n-0123456789abcdef";
    let boot = crate::durability::test_boot(1);
    let open = |path: &Path| {
        let state = StateDir::open(path).expect("open the state dir");
        state.set_inc_seam(IncSeam {
            boot: Some(Some(boot)),
            ..IncSeam::default()
        });
        state
    };
    let (a, c) = (scratch("mark-a"), scratch("mark-c"));
    let original = open(&a);
    std::fs::write(a.join("node"), format!("{NODE}\n")).expect("the node");
    let mark = original
        .mark(NODE)
        .expect("a mark where the platform names an inode");
    assert_eq!(mark.len(), 16, "{mark}");
    assert!(mark.bytes().all(|b| b.is_ascii_hexdigit()), "{mark}");
    assert_eq!(
        open(&a).mark(NODE).as_deref(),
        Some(mark.as_str()),
        "the same dir, opened again, is the same dir"
    );
    let copy = open(&c);
    std::fs::copy(a.join("node"), c.join("node")).expect("copy the node");
    let copied = copy.mark(NODE).expect("the copy's mark");
    assert_ne!(copied, mark, "a copy of the dir is another dir");
    assert_ne!(
        original.mark("n-fedcba9876543210").as_deref(),
        Some(mark.as_str()),
        "another node's"
    );
    original.set_inc_seam(IncSeam {
        boot: Some(Some(crate::durability::test_boot(2))),
        ..IncSeam::default()
    });
    assert_ne!(original.mark(NODE).as_deref(), Some(mark.as_str()));
    original.set_inc_seam(IncSeam {
        boot: Some(None),
        ..IncSeam::default()
    });
    assert_eq!(
        original.mark(NODE),
        None,
        "no boot identity grants no adoption"
    );
    original.set_inc_seam(IncSeam {
        boot: Some(Some(boot)),
        ..IncSeam::default()
    });
    assert_eq!(original.mark(NODE).as_deref(), Some(mark.as_str()));
    let moved = scratch("mark-moved");
    std::fs::rename(&a, &moved).expect("rename the original");
    assert_eq!(
        open(&moved).mark(NODE).as_deref(),
        Some(mark.as_str()),
        "a rename keeps the dir"
    );
    let of_bytes = |dir: &Path| {
        let node = std::fs::read(dir.join("node")).expect("the node");
        format!("{:016x}", aterm_hash::fnv1a_64(&node))
    };
    assert_eq!(
        of_bytes(&moved),
        of_bytes(&c),
        "the copy holds the same bytes"
    );
    let _ = std::fs::remove_dir_all(&moved);
    let _ = std::fs::remove_dir_all(&c);
}

#[test]
fn the_shipped_trust_decision_leases_a_dir_on_the_boot_volume() {
    let dir = scratch("inclease-shipped");
    let st = StateDir::open(&dir).expect("open");
    let boot = crate::durability::this_boot();
    let on_boot = on_boot_volume(&dir);
    if cfg!(target_vendor = "apple") {
        assert!(boot.is_some(), "macOS names its boot session");
        assert!(on_boot, "{dir:?} is on the boot volume");
    }
    let before = st.ops();
    let taken: Vec<u64> = (0..5)
        .map(|_| st.take_incarnation(0).expect("take"))
        .collect();
    assert_eq!(taken, [1, 2, 3, 4, 5]);
    let fsyncs = (st.ops() - before).fsyncs;
    eprintln!(
        "MEASURED five shipped takes: {fsyncs} fsyncs (boot {boot:?}, on boot volume {on_boot})"
    );
    let want = if boot.is_some() && on_boot { 2 } else { 10 };
    assert_eq!(fsyncs, want, "five takes on the shipped trust decision");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The seam picks the dir the shipped volume probe opens, never its answer: the dirs it picks
/// answer as it says, so every test that stands a dir off the boot volume reaches the probe a
/// shipped build runs, and a probe that answered `true` off it fails them.
#[test]
fn the_seam_stands_a_dir_off_the_boot_volume_through_the_shipped_probe() {
    assert!(on_boot_volume(Path::new("/")), "/ is the boot volume");
    assert!(
        !on_boot_volume(Path::new("/dev")),
        "/dev is a device filesystem"
    );
    let dir = scratch("inclease-probe");
    let st = StateDir::open(&dir).expect("open");
    st.set_inc_seam(IncSeam {
        boot: Some(Some(boot_id(1))),
        ..IncSeam::default()
    });
    assert_eq!(st.trusted_boot(), Some(boot_id(1)), "on the boot volume");
    st.stand_off_the_boot_volume();
    assert_eq!(st.trusted_boot(), None, "off the boot volume");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The shipped volume probe opens the state dir itself, with no seam: a state dir reached
/// through a link is judged by the dir the link points at, so pointed at `/` it is trusted in
/// this boot and pointed at `/dev` it is not. A probe that opened any fixed dir in place of this
/// one, or answered one way everywhere, fails one of the two wherever the kernel names its boot
/// (macOS and Linux, which the test asserts).
#[test]
fn the_shipped_volume_probe_judges_the_dir_itself() {
    let (real, link) = (scratch("inclease-real"), scratch("inclease-link"));
    std::fs::create_dir_all(&real).expect("real");
    std::os::unix::fs::symlink(&real, &link).expect("link");
    let st = StateDir::open(&link).expect("open");
    let boot = crate::durability::this_boot();
    if cfg!(any(target_vendor = "apple", target_os = "linux")) {
        assert!(boot.is_some(), "the kernel names its boot");
    }
    let point_at = |dir: &str| {
        std::fs::remove_file(&link).expect("unlink");
        std::os::unix::fs::symlink(dir, &link).expect("relink");
    };
    point_at("/");
    assert_eq!(st.trusted_boot(), boot, "a dir on the boot volume");
    point_at("/dev");
    assert_eq!(st.trusted_boot(), None, "a dir off the boot volume");
    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_dir_all(&real);
}

fn lay_down(st: &StateDir, s: &aterm_spec::interp::State) {
    let v = |name: &str| u64::try_from(s[name]).expect("a model value is not negative");
    let put = |file: &str, held: Option<String>| match held {
        Some(text) => std::fs::write(st.path(file), text).expect(file),
        None => match std::fs::remove_file(st.path(file)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => panic!("{file}: {e}"),
            _ => {}
        },
    };
    put("inc", (v("c_inc") > 0).then(|| v("c_inc").to_string()));
    put(
        "inc-lease",
        (v("c_lb") > 0).then(|| {
            IncLease {
                used: v("c_used"),
                top: v("c_top"),
                boot: Some(boot_id(u8::try_from(v("c_lb")).expect("a small boot"))),
                file: st.inc_file(),
            }
            .render()
        }),
    );
    put(
        "seq",
        (v("c_seq") > 0).then(|| (base(v("c_seq")) + 64).to_string()),
    );
}

fn read_back(st: &StateDir) -> [i64; 5] {
    let int = |n: u64| i64::try_from(n).expect("a small value");
    let (used, top, lb) = st.lease().map_or((0, 0, 0), |l| {
        let boot = l.boot.expect("the walk always names its boot");
        assert!(boot.iter().all(|b| *b == boot[0]), "a stand-in boot id");
        (l.used, l.top, u64::from(boot[0]))
    });
    [
        int(st.incarnation_top()),
        int(used),
        int(top),
        int(lb),
        int(st.sequence() >> 32),
    ]
}

fn diverging_takes(
    model: &aterm_spec::derive::Model,
    states: &[aterm_spec::interp::State],
    mutant: u8,
    first: bool,
) -> (usize, Vec<String>) {
    let constant = |name: &str| {
        model
            .consts
            .iter()
            .find(|(declared, _)| *declared == name)
            .map(|(_, v)| u64::try_from(*v).expect("a constant that is not negative"))
            .unwrap_or_else(|| panic!("the model declares {name}"))
    };
    let (span, off_volume) = (constant("Span"), constant("OffVolume") == 1);
    let walk = if first { "control" } else { "walk" };
    let volume = if off_volume { "off" } else { "on" };
    let dir = scratch(&format!("inclease-{walk}-{volume}-{mutant}"));
    let st = StateDir::open(&dir).expect("open");
    let mut seen = std::collections::BTreeSet::new();
    let (mut compared, mut diverged) = (0, Vec::new());
    for s in states {
        let key = ["boot", "c_inc", "c_used", "c_top", "c_lb", "c_seq", "pc"].map(|k| s[k]);
        if s["pc"] != 1 || !seen.insert(key) {
            continue;
        }
        let completed = ["Take", "TakeFlushed"]
            .iter()
            .find_map(|a| model.successors(a, s).into_iter().next());
        for (kill, action) in [
            (None, None),
            (Some(IncStep::Renamed), Some("KillRenamed")),
            (Some(IncStep::Flushed), Some("KillFlushed")),
        ] {
            let killed = action.and_then(|a| model.successors(a, s).into_iter().next());
            let (expect, dies) = match (killed, &completed) {
                (Some(k), _) => (k, true),
                (None, Some(c)) => (c.clone(), false),
                (None, None) => continue, // the model's bound, not a step
            };
            if kill.is_some() && !dies && s["tr"] == 0 {
                // Distinct branch.
                continue;
            }
            lay_down(&st, s);
            st.set_inc_seam(IncSeam {
                boot: Some(Some(boot_id(u8::try_from(s["boot"]).expect("small")))),
                kill_at: kill,
                mutant,
                span: Some(span),
                off_volume,
            });
            let got = st.take_incarnation(0);
            let files = read_back(&st);
            let want = ["c_inc", "c_used", "c_top", "c_lb", "c_seq"].map(|k| expect[k]);
            let answer_ok = match (&got, dies) {
                (Err(_), true) => true,
                (Ok(inc), false) => i64::try_from(*inc).ok() == Some(s["pn"]),
                _ => false,
            };
            compared += 1;
            if !answer_ok || files != want {
                diverged.push(format!(
                    "from {s:?} killed at {kill:?}: took {got:?} (model pn {}), files \
                     {files:?}, model {want:?}",
                    s["pn"]
                ));
                if first {
                    let _ = std::fs::remove_dir_all(&dir);
                    return (compared, diverged);
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    (compared, diverged)
}

/// The take refines `IncarnationLease` on the boot volume and off it (`OffVolume = 1`, where
/// a disk vanishes within one boot): each defect of the take matches the model's own `Buggy`,
/// and every control, a defect walked against the committed model, diverges from it. Off the
/// boot volume the controls are 3 and 5, the defects that differ there: 1 and 2 bend a trust
/// that no boot gives there, and 5 trusts by the boot the lease names.
#[test]
fn the_incarnation_lease_refines_its_model() {
    use aterm_spec::interp::{reachable_states, with_consts};
    let on = aterm_models_session::incarnation_lease_model();
    let off = with_consts(&on, &[("OffVolume", 1)]);
    let defective = |config: &aterm_spec::derive::Model, buggy: u8| match buggy {
        0 => config.clone(),
        _ => with_consts(config, &[("Buggy", i64::from(buggy)), ("MaxBoot", 2)]),
    };
    let walks: Vec<_> = [(&on, 0), (&on, 1), (&on, 2), (&on, 3)]
        .into_iter()
        .chain([(&off, 0), (&off, 3), (&off, 5)])
        .map(|(config, buggy)| (config, defective(config, buggy), buggy))
        .collect();
    let roomier = |config| with_consts(config, &[("MaxTop", 7), ("MaxBoot", 2)]);
    let (roomier_on, roomier_off) = (roomier(&on), roomier(&off));
    let controls = [
        (&roomier_on, 1),
        (&roomier_on, 2),
        (&roomier_on, 3),
        (&roomier_off, 3),
        (&roomier_off, 5),
    ];
    let volume = |config: &aterm_spec::derive::Model| {
        if config.consts.contains(&("OffVolume", 1)) {
            "off the boot volume"
        } else {
            "on the boot volume"
        }
    };
    let found = std::thread::scope(|scope| {
        let runs: Vec<_> = walks
            .iter()
            .map(|(config, model, buggy)| {
                let run = scope
                    .spawn(move || diverging_takes(model, &reachable_states(model), *buggy, false));
                (false, volume(config), *buggy, run)
            })
            .chain(controls.iter().map(|(model, mutant)| {
                let run = scope
                    .spawn(move || diverging_takes(model, &reachable_states(model), *mutant, true));
                (true, volume(model), *mutant, run)
            }))
            .collect();
        runs.into_iter()
            .map(|(control, volume, n, run)| {
                let found = run
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
                (control, volume, n, found)
            })
            .collect::<Vec<_>>()
    });
    for (control, volume, n, (compared, diverged)) in found {
        if control {
            assert!(
                !diverged.is_empty(),
                "the take's defect {n} matched the committed model {volume} in all \
                 {compared} takes"
            );
            continue;
        }
        assert!(
            compared > 100,
            "Buggy = {n} {volume}: only {compared} takes compared"
        );
        assert!(
            diverged.is_empty(),
            "Buggy = {n} {volume}: the take is not the model's in {} of {compared}:\n{}",
            diverged.len(),
            diverged.join("\n")
        );
        eprintln!("IncarnationLease Buggy = {n} {volume}: {compared} takes held to the model");
    }
}

fn resume_through(
    st: &StateDir,
    s: &std::collections::BTreeMap<&'static str, i64>,
    top: i64,
    rebase_at_head: bool,
) -> i64 {
    let at = |name: &str| u64::try_from(s[name]).expect("a model offset");
    let (g, rg, h, c) = (at("g"), at("rg"), at("h"), at("c"));
    let mut steps = Vec::new();
    if g >= 2 && s["f2"] != top {
        steps.push((g - 1, at("f2")));
    }
    if g >= 1 && s["f1"] != top {
        steps.push((g, at("f1")));
    }
    let lineage = astream_broker::Lineage::from_steps(&steps).expect("a lineage astream reads");
    let log = |generation| astream_broker::LogIdentity {
        id: [0x5a; 16],
        generation,
    };
    let witness = LogWitness {
        high: c,
        id: Some(log(rg)),
    };
    let keep_below = if rebase_at_head {
        None
    } else {
        witness.continues(Some(log(g)), Some(&lineage))
    };
    let mut cursors = BTreeMap::new();
    cursors.insert("t".to_string(), TopicCursor { next: c });
    st.set_topics("s-r", &cursors).expect("write the cursor");
    st.forget_log_offsets(h, Some(log(g)), keep_below, Some(&lineage))
        .expect("forget or clamp");
    i64::try_from(st.topics("s-r")["t"].next).expect("an offset")
}

#[test]
fn the_reader_resume_refines_its_model() {
    use aterm_spec::interp;
    let committed = aterm_models_session::fabric_reader_resume_model();
    let top = committed
        .consts
        .iter()
        .find(|(name, _)| *name == "Top")
        .map(|(_, v)| *v)
        .expect("the model's Top");
    let dir = scratch("resume-tier1");
    let st = StateDir::open(&dir).expect("open");
    let mut real: BTreeMap<[i64; 6], i64> = BTreeMap::new();
    let mut run = |s: &std::collections::BTreeMap<&'static str, i64>| {
        let key = ["g", "rg", "h", "c", "f1", "f2"].map(|v| s[v]);
        *real
            .entry(key)
            .or_insert_with(|| resume_through(&st, s, top, false))
    };
    let mut walk = |model: &aterm_spec::derive::Model, committed: bool| {
        let (mut compared, mut diverged, mut rebase_refused) = (0, Vec::new(), false);
        for state in interp::reachable_states(model) {
            let deciding = state["pc"] == 0 && state["down"] == 0 && state["rg"] < state["g"];
            if !deciding {
                continue;
            }
            let cursor = run(&state);
            let decided = |r| {
                let mut next = state.clone();
                next.insert("r", r);
                next.insert("pc", 1);
                next
            };
            compared += 1;
            let taken = decided(cursor);
            if interp::admits(model, &state, &taken) == Some("Decide") {
                let mut resumed = taken.clone();
                assert!(model.fire("Resume", &mut resumed), "Resume from {taken:?}");
                for law in ["NoSkipInWindow", "AnotherLogOutsideWindow"]
                    .into_iter()
                    .filter(|_| committed)
                {
                    assert!(
                        model.check_invariant(law, &resumed),
                        "{}: the real resume to {cursor} breaks {law} from {state:?}",
                        model.name
                    );
                }
            } else {
                diverged.push(format!("{state:?}: real {cursor}"));
                if !committed {
                    break;
                }
            }
            if committed && !rebase_refused {
                let rebase = resume_through(&st, &state, top, true);
                rebase_refused = interp::admits(model, &state, &decided(rebase)) != Some("Decide");
            }
        }
        (compared, diverged, rebase_refused)
    };

    let (compared, diverged, rebase_refused) = walk(&committed, true);
    eprintln!("FabricReaderResume: {compared} resumes held to the model");
    assert!(compared > 100, "only {compared} resumes compared");
    assert!(
        diverged.is_empty(),
        "the real reader is not the model's in {} of {compared}:\n{}",
        diverged.len(),
        diverged.join("\n")
    );
    assert!(
        rebase_refused,
        "NEGATIVE CONTROL: the reader that re-bases at the head matched the model in all \
         {compared} resumes"
    );
    for buggy in 1..=3 {
        let (compared, diverged, _) = walk(&interp::with_buggy(&committed, buggy), false);
        assert!(
            !diverged.is_empty(),
            "the real reader matched the defective model Buggy = {buggy} in all {compared} \
             resumes"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_generation_clamp_preserves_uncertain_refill_evidence() {
    let dir = crate::test_scratch_dir("clamp");
    let st = StateDir::open(dir.path()).expect("open");
    let sid = "s-kept";
    let topics = BTreeMap::from([("topic".to_string(), TopicCursor { next: 9 })]);
    st.set_topics(sid, &topics).unwrap();
    st.set_seen_off(sid, 8).unwrap();
    st.set_topic_floors(
        sid,
        &BTreeMap::from([("topic".to_string(), 1), ("gone".to_string(), 7)]),
    )
    .unwrap();
    st.forget_log_offsets(9, None, Some(6), None).unwrap();
    assert_eq!(
        st.topic_floors(sid).unwrap(),
        BTreeMap::from([("topic".into(), 1)])
    );
    assert_eq!(st.topics(sid)["topic"].next, 6);
    assert_eq!(st.seen_off(sid), Some(5));
    st.forget_log_offsets(9, None, None, None).unwrap();
    assert!(st.topic_floors(sid).unwrap().is_empty());

    let mut preserved = Vec::new();
    for keep in [Some(6), None] {
        for bytes in [b"not a floor".as_slice(), b"", b"topic 1\ntopic 2", b"\xff"] {
            st.set_topics(sid, &topics).unwrap();
            st.set_seen_off(sid, 8).unwrap();
            let floor = dir.path().join("unhandled").join(sid);
            std::fs::write(&floor, bytes).unwrap();
            let before_topics = std::fs::read(dir.path().join("topics").join(sid)).unwrap();
            let before_seen = std::fs::read(dir.path().join("seen").join(sid)).unwrap();
            let result = st.forget_log_offsets(9, None, keep, None);
            preserved.push(
                result.is_err()
                    && std::fs::read(&floor).is_ok_and(|got| got == bytes)
                    && std::fs::read(dir.path().join("topics").join(sid))
                        .is_ok_and(|got| got == before_topics)
                    && std::fs::read(dir.path().join("seen").join(sid))
                        .is_ok_and(|got| got == before_seen),
            );
        }
    }
    assert!(
        preserved.into_iter().all(|kept| kept),
        "CLAMP_DROPPED_UNCERTAIN_REFILL_EVIDENCE"
    );
}

fn in_flight_entry(seq: u64) -> InFlight {
    InFlight {
        pin: PublishPin {
            seq,
            via_key: false,
            identity: None,
            original: None,
            original_off: None,
            owner: None,
            at: None,
        },
        subject: "/f/sim/in/n-1/s-a/n-1/note".to_string(),
        body: b"v=1 t=5 text=hello".to_vec(),
        log: astream_broker::LogIdentity::parse(&format!("{}.3", "5a".repeat(16))),
        at: None,
    }
}

#[test]
fn a_torn_file_after_an_unclean_stop_is_set_aside_and_within_a_boot_refused() {
    for unclean in [true, false] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("torn")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(in_boot(1));
        st.begin_run().expect("the last bridge's run, in boot 1");
        st.take_incarnation(0).expect("its attach");
        let whole = in_flight_entry(7);
        st.set_in_flight(&whole).expect("a whole entry");
        let pin = PublishPin {
            identity: Some(PublishIdentity {
                subject: whole.subject.clone(),
                digest: "ab".repeat(32),
            }),
            ..whole.pin.clone()
        };
        st.set_post_pin("s-a", 1, &pin)
            .expect("a whole reservation");
        let journal_path = root.join(journal::FILE);
        let whole_journal = std::fs::read(&journal_path).expect("the journal");
        let next_record = journal::encode(
            journal::read(&whole_journal).next_step,
            &journal::Frame::InFlight(in_flight_entry(10)),
        )
        .expect("frames");
        let mut torn_journal = whole_journal.clone();
        torn_journal.extend_from_slice(&next_record);
        let last = torn_journal.len() - 1;
        torn_journal[last] ^= 0x01;
        std::fs::write(&journal_path, &torn_journal).expect("a record torn inside");
        let mut cut_journal = whole_journal.clone();
        cut_journal.extend_from_slice(&next_record[..next_record.len() / 2]);
        std::fs::create_dir_all(root.join("inflight")).expect("an older build's");
        std::fs::create_dir_all(root.join("sent")).expect("an older build's");
        let entry = in_flight_entry(8).render();
        let cut_entry = &entry.as_bytes()[..entry.find("/s-a/").expect("the face") + 3];
        std::fs::write(root.join("inflight/8"), cut_entry).expect("cut entry");
        let reservation = PublishPin {
            seq: 9,
            ..pin.clone()
        }
        .render();
        let cut_pin = &reservation.as_bytes()[..reservation.len() - 20];
        std::fs::write(root.join("sent/s-a.2"), cut_pin).expect("cut reservation");
        for (place, name) in [
            ("acks", "s-a.4"),
            ("keys", "s-a"),
            ("unhandled", "s-a"),
            ("", "logpin"),
        ] {
            std::fs::create_dir_all(root.join(place)).expect("its dir");
            std::fs::write(root.join(place).join(name), b"").expect("torn to nothing");
        }
        std::fs::write(root.join("keys/s-a"), b"k1 12 identity /f/x").expect("cut key");

        if unclean {
            st.set_inc_seam(in_boot(2));
            assert!(
                st.torn_files_possible().expect("run record reads"),
                "boot 2, after boot 1's bridge stopped uncleanly"
            );
            let moved = st.set_aside_torn().expect("the sweep");
            let from: Vec<&str> = moved.iter().map(|m| m.from.as_str()).collect();
            assert_eq!(
                from,
                [
                    "inflight/8",
                    "sent/s-a.2",
                    "acks/s-a.4",
                    "keys/s-a",
                    "unhandled/s-a",
                    "journal/posts",
                ],
                "every torn file, kind by kind"
            );
            for m in &moved {
                assert_eq!(m.to, format!("{QUARANTINE}/{}", m.from.replace('/', ".")));
                assert_eq!(
                    root.join(&m.from).exists(),
                    m.from == journal::FILE,
                    "{} is moved, not copied; the journal is rewritten whole",
                    m.from
                );
            }
            assert_eq!(
                std::fs::read(root.join("quarantine/inflight.8")).expect("kept"),
                cut_entry,
                "its exact bytes, never deleted"
            );
            assert_eq!(
                std::fs::read(root.join("quarantine/sent.s-a.2")).expect("kept"),
                cut_pin
            );
            assert_eq!(
                std::fs::read(root.join("quarantine/journal.posts")).expect("kept"),
                torn_journal,
                "the journal's exact bytes, its torn record with them"
            );
            assert_eq!(
                journal::read(&std::fs::read(&journal_path).expect("rewritten")).end,
                journal::End::Clean,
                "the journal holds what its whole records said"
            );
            assert_eq!(
                st.in_flight().expect("the journal reads"),
                vec![(7, whole.clone())]
            );
            assert_eq!(st.post_pin("s-a", 2).expect("reads"), None);
            assert_eq!(st.post_pin("s-a", 1).expect("reads"), Some(pin.clone()));
            assert_eq!(st.receipt_pin("s-a", 4).expect("reads"), None);
            assert!(st.topic_floors("s-a").expect("reads").is_empty());
            assert_eq!(
                st.logpin().expect("reads").as_deref(),
                Some(""),
                "the mirror's pin stays, for the mirror to refuse"
            );
            st.publication_offset_resets(None)
                .expect("a log move's reset reads");
            assert!(st.set_aside_torn().expect("again").is_empty(), "idempotent");
            std::fs::write(&journal_path, &cut_journal).expect("a record cut short");
            assert!(
                st.set_aside_torn().expect("a cut record").is_empty(),
                "a record cut short stays, for its reader to drop"
            );
            std::fs::write(root.join("inflight/8"), b"").expect("torn again");
            assert_eq!(
                st.publish_quarantine_file("inflight/8", "quarantine/inflight.8", b"replacement",)
                    .expect_err("the actual publication cannot replace an occupant")
                    .kind(),
                io::ErrorKind::AlreadyExists
            );
            assert_eq!(
                std::fs::read(root.join("inflight/8")).expect("source remains"),
                b""
            );
            assert_eq!(
                std::fs::read(root.join("quarantine/inflight.8")).expect("sentinel remains"),
                cut_entry
            );
            let again = st.set_aside_torn().expect("the next boot's sweep");
            assert_eq!(again.len(), 1);
            assert_eq!(again[0].to, format!("{QUARANTINE}/inflight.8.1"));
            assert_eq!(
                std::fs::read(root.join("quarantine/inflight.8")).expect("kept"),
                cut_entry,
                "the first one is not overwritten"
            );
        } else {
            assert!(
                !st.torn_files_possible().expect("run record reads"),
                "the same boot: nothing a kill does tears a file"
            );
            assert!(st.set_aside_torn().expect("no sweep").is_empty());
            assert!(!root.join(QUARANTINE).exists(), "nothing moved");
            let next = StateDir::open(root).expect("the next bridge of the boot");
            next.set_inc_seam(in_boot(1));
            assert_eq!(
                next.in_flight().expect_err("refused").kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(
                next.post_pin("s-a", 2).expect_err("refused").kind(),
                io::ErrorKind::InvalidData
            );
            assert!(next.publication_offset_resets(None).is_err());
            assert!(next.topic_floors("s-a").is_err());
            assert!(next
                .logpin()
                .expect("read")
                .is_some_and(|t| crate::mirror::Pin::parse(&t).is_err()));
            for older in ["inflight/8", "sent/s-a.2"] {
                std::fs::remove_file(root.join(older)).expect("repaired");
            }
            assert_eq!(
                next.in_flight().expect_err("refused").kind(),
                io::ErrorKind::InvalidData,
                "a record torn inside is damage within a boot"
            );
            assert_eq!(
                std::fs::read(&journal_path).expect("kept"),
                torn_journal,
                "kept for repair"
            );
            std::fs::write(&journal_path, &cut_journal).expect("a record cut short");
            let next = StateDir::open(root).expect("the next bridge of the boot");
            next.set_inc_seam(in_boot(1));
            assert_eq!(
                next.in_flight().expect("the journal reads"),
                vec![(7, whole.clone())],
                "a record cut short within a boot is an append that failed: the records \
                 before it"
            );
            assert_eq!(next.post_pin("s-a", 1).expect("reads"), Some(pin.clone()));
        }
    }
}

#[test]
fn a_planted_quarantine_link_is_refused_and_nothing_moves() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("planted")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let elsewhere = aterm_tempfile::Builder::new()
        .prefix("elsewhere")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = dir.path();
    let st = StateDir::open(root).expect("open");
    st.set_inc_seam(in_boot(1));
    st.begin_run()
        .expect("a run in boot 1, never stopped cleanly");
    st.set_inc_seam(in_boot(2));
    std::fs::create_dir_all(root.join("acks")).expect("its dir");
    std::fs::write(root.join("acks/s-a.1"), b"").expect("torn to nothing");
    std::os::unix::fs::symlink(elsewhere.path(), root.join(QUARANTINE)).expect("plant");
    let refused = st.set_aside_torn().expect_err("a link is no quarantine");
    assert_eq!(refused.kind(), io::ErrorKind::InvalidData);
    assert!(root.join("acks/s-a.1").exists(), "nothing moved");
    assert_eq!(
        std::fs::read_dir(elsewhere.path()).expect("list").count(),
        0,
        "nothing written through the link"
    );
}

#[test]
fn a_sweep_tried_again_after_its_unlinks_failed_moves_each_file_once() {
    use std::os::unix::fs::PermissionsExt as _;
    assert_ne!(
        aterm_uds::peer::our_uid(),
        0,
        "chmod refusal needs an unprivileged uid"
    );
    struct RestoreMode(PathBuf, std::fs::Permissions);
    impl Drop for RestoreMode {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, self.1.clone());
        }
    }
    for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::Unsupported] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("sweep-again-parent")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).unwrap();
        st.set_inc_seam(in_boot(1));
        st.begin_run().unwrap();
        std::fs::create_dir_all(root.join("inflight")).unwrap();
        std::fs::write(root.join("inflight/7"), b"torn 7").unwrap();
        st.set_inc_seam(in_boot(2));
        st.refuse_directory_flush(Some((root, aterm_durable::Flush::Durable, kind)));
        let before = st.ops();
        for _ in 0..2 {
            assert_eq!(st.set_aside_torn().unwrap_err().kind(), kind);
            assert!(root.join(QUARANTINE).is_dir());
            assert_eq!(std::fs::read_dir(root.join(QUARANTINE)).unwrap().count(), 0);
            assert_eq!(std::fs::read(root.join("inflight/7")).unwrap(), b"torn 7");
            assert_eq!(st.ops().replaces, before.replaces, "no copy published");
        }
        assert!(
            st.ops().fsyncs > before.fsyncs,
            "the root fence was attempted"
        );
        st.refuse_directory_flush(None);
        let moved = st.set_aside_torn().unwrap();
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].to, "quarantine/inflight.7");
        assert_eq!(std::fs::read(root.join(&moved[0].to)).unwrap(), b"torn 7");
        assert!(!root.join("inflight/7").exists());
    }
    let dir = aterm_tempfile::Builder::new()
        .prefix("sweep-again")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = dir.path();
    let st = StateDir::open(root).expect("open");
    st.set_inc_seam(in_boot(1));
    st.begin_run()
        .expect("a run in boot 1, never stopped cleanly");
    std::fs::create_dir_all(root.join("inflight")).expect("its dir");
    for seq in [7, 8, 9] {
        std::fs::write(root.join(format!("inflight/{seq}")), format!("torn {seq}")).expect("torn");
    }
    std::fs::create_dir_all(root.join(QUARANTINE)).expect("the quarantine");
    std::fs::write(root.join("quarantine/inflight.7"), b"torn 7")
        .expect("a person's same-byte occupant");
    st.set_inc_seam(in_boot(2));
    st.refuse_writes_of(Some("quarantine/inflight.7"));
    assert!(
        st.set_aside_torn().is_err(),
        "the first publish failed before its link"
    );
    st.refuse_writes_of(None);
    assert_eq!(std::fs::read(root.join("inflight/7")).unwrap(), b"torn 7");
    st.refuse_directory_flush(Some((
        root.join(QUARANTINE).as_path(),
        aterm_durable::Flush::Durable,
        io::ErrorKind::Other,
    )));
    assert!(
        st.set_aside_torn().is_err(),
        "exclusive link made, its fence failed"
    );
    st.refuse_directory_flush(None);
    assert_eq!(
        std::fs::read(root.join("quarantine/inflight.7.1")).unwrap(),
        b"torn 7"
    );
    assert!(
        root.join("inflight/7").exists(),
        "no source unlinked after the failed fence"
    );
    std::fs::write(root.join("quarantine/inflight.9"), b"a person's own file")
        .expect("a file of other bytes at the name");
    st.set_inc_seam(in_boot(2));
    let mode = |m: u32| {
        std::fs::set_permissions(root.join("inflight"), std::fs::Permissions::from_mode(m))
            .expect("chmod");
    };
    let _restore = RestoreMode(
        root.join("inflight"),
        std::fs::metadata(root.join("inflight"))
            .unwrap()
            .permissions(),
    );
    mode(0o555);
    for attempt in 0..3 {
        let refused = st.set_aside_torn().expect_err("the unlinks fail");
        assert_eq!(
            refused.kind(),
            io::ErrorKind::PermissionDenied,
            "try {attempt}"
        );
    }
    mode(0o755);
    let moved = st.set_aside_torn().expect("the sweep, its dir writable");
    let to: Vec<&str> = moved.iter().map(|m| m.to.as_str()).collect();
    assert_eq!(
        to,
        [
            "quarantine/inflight.7.1",
            "quarantine/inflight.8",
            "quarantine/inflight.9.1"
        ]
    );
    let mut held: Vec<String> = std::fs::read_dir(root.join(QUARANTINE))
        .expect("list")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    held.sort();
    assert_eq!(
        held,
        [
            "inflight.7",
            "inflight.7.1",
            "inflight.8",
            "inflight.9",
            "inflight.9.1"
        ],
        "one copy of each torn file, and the person's file kept"
    );
    assert_eq!(
        std::fs::read(root.join("quarantine/inflight.9")).expect("kept"),
        b"a person's own file"
    );
    assert_eq!(
        std::fs::read(root.join("quarantine/inflight.7")).unwrap(),
        b"torn 7",
        "same-byte occupant kept, not adopted"
    );
    assert_eq!(st.quarantined_torn().expect("count"), Some(5));
    for seq in [7, 8, 9] {
        assert!(
            !root.join(format!("inflight/{seq}")).exists(),
            "{seq} moved"
        );
    }
    st.begin_run()
        .expect("a run in boot 2, never stopped cleanly");
    std::fs::write(root.join("inflight/7"), "torn 7").expect("torn again");
    st.set_inc_seam(in_boot(3));
    let again = st.set_aside_torn().expect("boot 3's sweep");
    let to: Vec<&str> = again.iter().map(|m| m.to.as_str()).collect();
    assert_eq!(
        to,
        ["quarantine/inflight.7.2"],
        "another file, another copy"
    );
    assert_eq!(st.quarantined_torn().expect("count"), Some(6));
    for replaced in [false, true] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("sweep-again-identity")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).unwrap();
        st.set_inc_seam(in_boot(1));
        st.begin_run().unwrap();
        std::fs::create_dir_all(root.join("inflight")).unwrap();
        std::fs::write(root.join("inflight/7"), b"torn 7").unwrap();
        st.set_inc_seam(in_boot(2));
        let quarantine = root.join(QUARANTINE);
        st.refuse_directory_flush(Some((
            quarantine.as_path(),
            aterm_durable::Flush::Durable,
            io::ErrorKind::Other,
        )));
        assert!(st.set_aside_torn().is_err());
        st.refuse_directory_flush(None);
        let copy = quarantine.join("inflight.7");
        std::fs::remove_file(&copy).unwrap();
        if replaced {
            std::fs::write(&copy, b"torn 7").unwrap();
        }
        let moved = st.set_aside_torn().unwrap();
        assert_eq!(moved.len(), 1);
        assert_eq!(
            moved[0].to,
            if replaced {
                "quarantine/inflight.7.1"
            } else {
                "quarantine/inflight.7"
            }
        );
        assert_eq!(std::fs::read(&copy).unwrap(), b"torn 7");
        assert!(!root.join("inflight/7").exists());
    }
}

#[test]
fn a_planted_source_parent_is_refused_before_sweep_or_clean_close() {
    for kind in TornKind::ALL {
        for linked in [true, false] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("source-parent")
                .suffix(".noindex")
                .tempdir_in("/tmp")
                .expect("scratch");
            let elsewhere = aterm_tempfile::Builder::new()
                .prefix("source-target")
                .suffix(".noindex")
                .tempdir_in("/tmp")
                .expect("scratch");
            let st = StateDir::open(dir.path()).expect("open");
            st.set_inc_seam(in_boot(1));
            st.begin_run().expect("open run");
            let name = match kind {
                TornKind::InFlight => "8",
                TornKind::Journal => journal::NAME,
                _ => "s-a.1",
            };
            let rel = format!("{}/{name}", kind.place());
            st.write(&rel, "12").expect("a pending write");
            std::fs::remove_file(st.path(&rel)).expect("remove owned fixture");
            std::fs::remove_dir(st.path(kind.place())).expect("replace owned parent");
            let sentinel = elsewhere.path().join(name);
            std::fs::write(&sentinel, b"external evidence").expect("outside sentinel");
            if linked {
                std::os::unix::fs::symlink(elsewhere.path(), st.path(kind.place()))
                    .expect("plant parent link");
            } else {
                std::fs::write(st.path(kind.place()), b"parent sentinel")
                    .expect("plant non-directory");
            }
            let run = std::fs::read(st.path("run")).expect("original run");
            st.set_inc_seam(in_boot(2));
            assert_eq!(
                st.set_aside_torn()
                    .expect_err("refuse source parent")
                    .kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(
                st.end_run_clean().expect_err("refuse clean close").kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(
                std::fs::read(&sentinel).expect("outside remains"),
                b"external evidence"
            );
            assert_eq!(std::fs::read(st.path("run")).expect("run remains"), run);
            assert!(st.unflushed.lock().expect("pending").contains(&rel));
            assert!(
                !st.path(QUARANTINE).exists(),
                "no evidence publication before admission"
            );
            if linked {
                assert!(std::fs::symlink_metadata(st.path(kind.place()))
                    .expect("link remains")
                    .is_symlink());
            } else {
                assert_eq!(
                    std::fs::read(st.path(kind.place())).expect("parent remains"),
                    b"parent sentinel"
                );
            }
        }
    }
}

#[test]
fn an_existing_run_marker_never_authorizes_legacy_fallback() {
    use std::os::unix::fs::PermissionsExt as _;
    for occupant in ["invalid", "directory", "dangling", "unreadable"] {
        // Distinct branch.
        if occupant == "unreadable" && aterm_uds::peer::our_uid() == 0 {
            // Distinct branch.
            continue;
        }
        let dir = aterm_tempfile::Builder::new()
            .prefix("run-marker")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let st = StateDir::open(dir.path()).expect("open");
        st.set_inc_seam(in_boot(1));
        st.take_incarnation(0).expect("legacy lease");
        st.set_inc_seam(in_boot(2));
        assert!(st
            .torn_files_possible()
            .expect("genuine absence uses legacy lease"));
        let run = st.path("run");
        let clean = LastRun {
            boot: Some(boot_id(1)),
            clean: true,
        }
        .render();
        match occupant {
            "invalid" => std::fs::write(&run, b"not a run\n").expect("invalid marker"),
            "directory" => std::fs::create_dir(&run).expect("nonregular marker"),
            "dangling" => std::os::unix::fs::symlink(dir.path().join("missing"), &run)
                .expect("dangling marker"),
            _ => {
                std::fs::write(&run, &clean).expect("valid clean marker");
                std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o000))
                    .expect("make unreadable");
            }
        }
        std::fs::create_dir_all(st.path("acks")).expect("its dir");
        std::fs::write(st.path("acks/s-a.1"), b"torn evidence").expect("candidate");
        assert!(st.last_run().is_err(), "the occupant is not absence");
        assert!(st.torn_files_possible().is_err(), "no legacy verdict");
        assert!(st.set_aside_torn().is_err(), "no sweep");
        assert!(st.begin_run().is_err(), "no replacement on start");
        assert!(st.end_run_clean().is_err(), "no replacement on stop");
        assert_eq!(
            std::fs::read(st.path("acks/s-a.1")).expect("source remains"),
            b"torn evidence"
        );
        assert!(!st.path(QUARANTINE).exists());
        match occupant {
            "invalid" => assert_eq!(
                std::fs::read(&run).expect("invalid evidence remains"),
                b"not a run\n"
            ),
            "directory" => assert!(std::fs::symlink_metadata(&run)
                .expect("directory remains")
                .is_dir()),
            "dangling" => assert_eq!(
                std::fs::read_link(&run).expect("link remains"),
                dir.path().join("missing")
            ),
            _ => {
                std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o600))
                    .expect("restore owned marker");
                assert_eq!(
                    std::fs::read_to_string(&run).expect("clean evidence remains"),
                    clean
                );
            }
        }
    }
}

#[test]
fn only_an_unclean_stop_in_an_earlier_boot_can_have_torn_a_file() {
    let unnamed = IncSeam {
        boot: Some(None),
        ..IncSeam::default()
    };
    for off_volume in [false, true] {
        let seam = |b| IncSeam {
            off_volume,
            ..in_boot(b)
        };
        let dir = aterm_tempfile::Builder::new()
            .prefix("unclean")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let st = StateDir::open(dir.path()).expect("open");
        st.set_inc_seam(seam(1));
        assert!(
            !st.torn_files_possible().expect("missing run"),
            "a new dir: nothing ran"
        );
        st.begin_run().expect("a run in boot 1");
        assert_eq!(
            st.last_run().expect("run record reads"),
            Some(LastRun {
                boot: Some(boot_id(1)),
                clean: false
            })
        );
        st.take_incarnation(0).expect("its attach");
        assert!(
            !st.torn_files_possible().expect("run reads"),
            "the same boot, killed"
        );
        st.set_inc_seam(seam(2));
        assert!(
            st.torn_files_possible().expect("run reads"),
            "boot 2, the run never closed"
        );
        st.set_inc_seam(unnamed);
        assert!(
            !st.torn_files_possible().expect("run reads"),
            "a boot nobody can name"
        );
        st.set_inc_seam(seam(1));
        st.end_run_clean().expect("a clean stop in boot 1");
        assert_eq!(
            st.last_run().expect("run record reads"),
            Some(LastRun {
                boot: Some(boot_id(1)),
                clean: true
            })
        );
        st.set_inc_seam(seam(2));
        assert!(
            !st.torn_files_possible().expect("run reads"),
            "boot 2, after a clean stop"
        );
        st.begin_run().expect("boot 2's run");
        st.set_inc_seam(seam(3));
        assert!(
            st.torn_files_possible().expect("run reads"),
            "boot 3, boot 2's run never closed"
        );
        std::fs::remove_file(dir.path().join("run")).expect("no record");
        st.set_inc_seam(seam(1));
        st.lease_names_no_boot_off_the_boot_volume(true);
        st.take_incarnation(0)
            .expect("an older build's attach, boot 1");
        st.set_inc_seam(seam(2));
        assert_eq!(
            st.torn_files_possible().expect("legacy missing run"),
            !off_volume,
            "the lease's boot, where an older build's names one"
        );
        st.set_inc_seam(seam(1));
        st.lease_names_no_boot_off_the_boot_volume(false);
        st.take_incarnation(0).expect("this build's attach, boot 1");
        st.set_inc_seam(seam(2));
        assert!(
            st.torn_files_possible().expect("missing run"),
            "this build's lease names its boot on every volume (RZ2)"
        );
    }
}

#[test]
fn the_quarantine_counts_the_torn_files_the_owner_has_not_taken_away() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("held")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = dir.path();
    let st = StateDir::open(root).expect("open");
    assert_eq!(st.quarantined_torn().expect("no quarantine reads"), None);
    st.set_inc_seam(in_boot(1));
    st.begin_run().expect("boot 1's run");
    st.set_in_flight(&in_flight_entry(7))
        .expect("a whole entry");
    let entry = in_flight_entry(8).render();
    std::fs::create_dir_all(root.join("inflight")).expect("an older build's");
    std::fs::write(
        root.join("inflight/8"),
        &entry.as_bytes()[..entry.find("/s-a/").expect("the face") + 3],
    )
    .expect("a cut entry");
    st.set_inc_seam(in_boot(2));
    assert_eq!(st.set_aside_torn().expect("the sweep").len(), 1);
    assert_eq!(
        st.retire_in_flight_of_an_earlier_boot()
            .expect("the retirement")
            .len(),
        1
    );
    let quarantine = root.join(QUARANTINE);
    std::fs::write(quarantine.join(".inflight.9.tmp.1"), b"staged").expect("a temp");
    std::os::unix::fs::symlink("inflight.8", quarantine.join("a-link")).expect("a link");
    assert_eq!(
        st.quarantined_torn().expect("the count"),
        Some(1),
        "the torn entry alone"
    );
    std::fs::remove_file(quarantine.join("inflight.8")).expect("the owner removes it");
    assert_eq!(
        st.quarantined_torn().expect("the count"),
        Some(0),
        "none torn left, the retired entry kept"
    );
    assert!(quarantine
        .join(format!("{RETIRED_PREFIX}inflight.7"))
        .exists());
    std::fs::remove_dir_all(&quarantine).expect("the owner empties it");
    std::fs::write(&quarantine, b"planted").expect("a file at the name");
    assert_eq!(
        st.quarantined_torn().expect_err("not a directory").kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn a_start_is_ordered_by_the_barrier_only_over_an_earlier_boots_record() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("start-tier")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = dir.path();
    let start = |boot: IncSeam| {
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(boot);
        let before = st.ops();
        let flushed = st.begin_run().expect("a start");
        let ops = st.ops() - before;
        assert_eq!(
            (ops.barriers, ops.fsyncs),
            (flushed.ordered, flushed.full),
            "said as made"
        );
        (st, (flushed.ordered, flushed.full))
    };
    let (_, cost) = start(in_boot(1));
    assert_eq!(cost, (0, 2), "over no record");
    let (_, cost) = start(in_boot(1));
    assert_eq!(
        cost,
        (0, 0),
        "over an open of this boot with its proof: nothing written (R6)"
    );
    let (st, _) = start(in_boot(1));
    std::fs::remove_file(st.proof_of_what_reads("run").expect("the record reads"))
        .expect("the proof the first start made");
    drop(st);
    let (_, cost) = start(in_boot(1));
    assert_eq!(cost, (0, 2), "over an open of this boot with no proof");
    let copy = root.join("run-copy");
    std::fs::copy(root.join("run"), &copy).expect("the record's bytes");
    std::fs::rename(&copy, root.join("run")).expect("the same bytes, another inode");
    let (st, cost) = start(in_boot(1));
    assert_eq!(
        cost,
        (0, 2),
        "over an open of this boot with no proof of its inode"
    );
    st.end_run_clean().expect("a clean stop in boot 1");
    let (_, cost) = start(in_boot(1));
    assert_eq!(cost, (0, 2), "over a clean stop of this boot");
    let (st, cost) = start(in_boot(2));
    assert_eq!(cost, (2, 0), "over an earlier boot's open");
    st.end_run_clean().expect("a clean stop in boot 2");
    let (_, cost) = start(in_boot(3));
    assert_eq!(cost, (2, 0), "over an earlier boot's clean stop");
    let (_, cost) = start(IncSeam {
        boot: Some(None),
        ..IncSeam::default()
    });
    assert_eq!(cost, (0, 2), "in a boot nobody can name");
    let (st, cost) = start(in_boot(4));
    assert_eq!(cost, (0, 2), "over a record whose boot nobody named");
    st.end_run_clean().expect("a clean stop in boot 4");
    let elsewhere = aterm_tempfile::Builder::new()
        .prefix("start-tier-link")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    std::os::unix::fs::symlink(elsewhere.path(), root.join(journal::DIR))
        .expect("a link where a directory goes");
    let (_, cost) = start(in_boot(5));
    assert_eq!(cost, (0, 2), "a torn kind's directory that does not read");
}

/// R6r: A PROOF SPEAKS ONLY FOR THE FILE AND THE DIRECTORY IT WAS MADE FOR, on real files (the
/// law under a power cut is `sim::tests`'
/// `no_copy_of_the_dir_nor_restore_over_it_proves_anything_under_a_power_cut`). A start in full
/// and a raise each leave one proof, under its file's name and none under a bare stem. Moved
/// on its volume the directory is the same one: its next start writes nothing and its next
/// raise flushes nothing. CONTROLS: a link under the bare stem, as R6's first builds (0.146,
/// 0.147) left one, or a name that differs from the file's own only by another directory's
/// inode, as a copy carries one in, is no proof: the start and the raise are in full, and each
/// such name goes as the next proof is made. A hand edit in place, its own bytes included, is
/// no proof either.
#[test]
fn a_proof_speaks_only_for_the_file_and_the_directory_it_was_made_for() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("proof-bound")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let first = dir.path().join("first");
    std::fs::create_dir(&first).expect("the state dir");
    let open = |root: &Path| {
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(in_boot(1));
        st
    };
    let held = |root: &Path| {
        let mut names: Vec<String> = std::fs::read_dir(root)
            .expect("the state dir reads")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(RUN_PROOF) || n.starts_with(LOGFLOOR_PROOF))
            .collect();
        names.sort();
        names
    };
    let its_own = |st: &StateDir| {
        [LOGFLOOR, "run"].map(|name| {
            let proof = st.proof_of_what_reads(name).expect("the file reads");
            assert!(identity_at(&proof).is_some(), "{name}: its proof stands");
            proof.file_name().unwrap().to_string_lossy().into_owned()
        })
    };
    let raise_cost = |st: &StateDir| {
        let before = st.ops().fsyncs;
        assert_eq!(st.raise_logfloor(50).expect("the raise"), 100);
        st.ops().fsyncs - before
    };
    {
        let st = open(&first);
        let began = st.begin_run().expect("the first start");
        assert_eq!((began.full, began.proven), (2, false), "in full");
        st.raise_logfloor(100).expect("the first floor");
        assert_eq!(held(&first), its_own(&st), "one proof a file, no bare stem");
    }
    let moved = dir.path().join("moved");
    std::fs::rename(&first, &moved).expect("the dir moved on its volume");
    let st = open(&moved);
    let began = st.begin_run().expect("the start after the move");
    assert_eq!(
        (began.ordered, began.full, began.proven),
        (0, 0, true),
        "the same directory: nothing written"
    );
    assert_eq!(raise_cost(&st), 0, "and its floor admitted");
    for (name, stem) in [("run", RUN_PROOF), (LOGFLOOR, LOGFLOOR_PROOF)] {
        std::fs::remove_file(st.proof_of_what_reads(name).expect("the file reads"))
            .expect("the directory's own proof");
        std::fs::hard_link(moved.join(name), moved.join(stem)).expect("an older build's link");
        let made = identity_at(&moved.join(name)).expect("the file reads");
        let (secs, nanos) = made.changed;
        let named = format!("{stem}.{}.1.{}.{secs}.{nanos}", made.dev, made.ino);
        std::fs::File::create(moved.join(named)).expect("another directory's proof of it");
    }
    drop(st);
    let st = open(&moved);
    let began = st.begin_run().expect("the start over stale proofs");
    assert_eq!(
        (began.ordered, began.full, began.proven),
        (0, 2, false),
        "CONTROL: an older build's link and another directory's are no proof"
    );
    assert_eq!(raise_cost(&st), 2, "CONTROL: nor for the floor");
    assert_eq!(held(&moved), its_own(&st), "each stale one gone");
    drop(st);
    let st = open(&moved);
    let began = st.begin_run().expect("the start after those writes");
    assert!(began.proven, "proved again by the writes in full");
    assert_eq!(raise_cost(&st), 0, "and its floor admitted");
    past_the_clock_tick(&moved);
    for name in ["run", LOGFLOOR] {
        let bytes = std::fs::read(moved.join(name)).expect("the file reads");
        std::fs::write(moved.join(name), bytes).expect("its own bytes, written into it");
    }
    drop(st);
    let st = open(&moved);
    let began = st.begin_run().expect("the start after a hand edit");
    assert_eq!(
        (began.ordered, began.full, began.proven),
        (0, 2, false),
        "CONTROL: a hand edit keeps the inode and moves its change time"
    );
    assert_eq!(raise_cost(&st), 2, "CONTROL: for the floor too");
}

/// R6r: A RESTORE WRITTEN INTO THE FILES A DIR HOLDS PROVES NOTHING, by the real `cp`, on real
/// files (the law under a power cut is `sim::tests`'
/// `no_copy_of_the_dir_nor_restore_over_it_proves_anything_under_a_power_cut`). `cp -R` and
/// `cp -a` keep the inode each name holds, as this test asserts first, and `cp -a` its
/// modification time, its own bytes included. GNU's `-a` also keeps hard links, so it replaces
/// a file that has another link (the control's proof is one); `--no-preserve=links` gives it
/// BSD's `-a` (`-pPR`). Over a bridge that died with its OPEN and floor proved, and over a
/// clean stop with a backup of a lower floor than its last raise: the next start and the next
/// raise are in full, and a successor's reading of the floor admits nothing.
/// CONTROL: a proof named for the inode alone ([`ProofRule::ThisInode`]) vouches for both.
#[test]
fn a_restore_written_into_the_files_a_dir_holds_proves_nothing() {
    let cp_binary = aterm_uds::deadline::stand_in(Path::new("/bin/cp"));
    let cp = |flag: &str, from: &Path, to: &Path| {
        let mut command = aterm_uds::spawnfd::command(&cp_binary);
        command.arg(flag);
        #[cfg(target_os = "linux")]
        if flag == "-a" {
            command.arg("--no-preserve=links");
        }
        let out = aterm_uds::deadline::output_until(
            command.arg(from.join(".")).arg(to),
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .expect("cp ran");
        assert!(out.status.success(), "cp {flag}: {out:?}");
    };
    let inodes = |root: &Path| {
        ["run", LOGFLOOR].map(|name| {
            use std::os::unix::fs::MetadataExt as _;
            std::fs::metadata(root.join(name)).expect("the file").ino()
        })
    };
    let raise_cost = |st: &StateDir, top: u64, reads: u64| {
        let before = st.ops().fsyncs;
        assert_eq!(st.raise_logfloor(top).expect("the raise"), reads);
        st.ops().fsyncs - before
    };
    for (rule, vouches) in [(ProofRule::Ships, false), (ProofRule::ThisInode, true)] {
        let flushes = |proved: bool| if proved { 0 } else { 2 };
        for flag in ["-R", "-a"] {
            let scratch = aterm_tempfile::Builder::new()
                .prefix("proof-cp")
                .suffix(".noindex")
                .tempdir_in("/tmp")
                .expect("scratch");
            let (state, backup) = (scratch.path().join("state"), scratch.path().join("bk"));
            for dir in [&state, &backup] {
                std::fs::create_dir(dir).expect("a dir");
            }
            let open = || {
                let st = StateDir::open(&state).expect("open");
                st.set_inc_seam(in_boot(1));
                st.set_proof_rule(rule);
                st
            };
            {
                let st = open();
                st.begin_run().expect("its start");
                st.raise_logfloor(100).expect("its floor");
            }
            cp(flag, &state, &backup);
            let kept = inodes(&state);
            past_the_clock_tick(&state);
            cp(flag, &backup, &state);
            assert_eq!(inodes(&state), kept, "cp {flag} writes into the file");
            {
                let st = open();
                let began = st.begin_run().expect("the start over the restore");
                assert_eq!(
                    (began.proven, began.ordered, began.full),
                    (vouches, 0, flushes(vouches)),
                    "{rule:?}, cp {flag}, over a bridge that died: its start"
                );
                assert_eq!(
                    raise_cost(&st, 50, 100),
                    flushes(vouches),
                    "{rule:?}, cp {flag}, over a bridge that died: its raise"
                );
                st.raise_logfloor(300).expect("a raise past the backup");
                st.end_run_clean().expect("its clean stop");
            }
            let kept = inodes(&state);
            past_the_clock_tick(&state);
            cp(flag, &backup, &state);
            assert_eq!(inodes(&state), kept, "cp {flag} writes into the file");
            let st = open();
            assert_eq!(st.logfloor().expect("it reads"), Some(100), "cp {flag}");
            assert_eq!(
                st.admit_logfloor_by_its_proof(),
                vouches,
                "{rule:?}, cp {flag}, over a clean stop: a lower floor than the last raise"
            );
            assert_eq!(
                raise_cost(&st, 50, 100),
                flushes(vouches),
                "{rule:?}, cp {flag}, over a clean stop: its raise"
            );
        }
    }
}

/// R6: AN OPEN THE BARRIER ORDERED IS PROVED ONLY ONCE A FULL FLUSH COVERS IT. The first start
/// of boot 2, over boot 1's clean stop, writes OPEN by the barrier and makes no proof; the
/// boot's first incarnation writes `inc` in full and makes the OPEN's proof, and the next
/// start of the boot then writes nothing. CONTROL: where that write of `inc` fails, no
/// proof is made and the next start writes OPEN in full.
#[test]
fn an_open_the_barrier_ordered_is_proved_by_the_incarnations_full_flush() {
    for inc_fails in [false, true] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("proof-inc")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let first = StateDir::open(root).expect("open");
        first.set_inc_seam(in_boot(1));
        first.begin_run().expect("boot 1's run");
        first.take_incarnation(0).expect("boot 1's incarnation");
        first.end_run_clean().expect("its clean stop");
        drop(first);
        let proved = |st: &StateDir| {
            st.proof_of_what_reads("run")
                .is_some_and(|proof| identity_at(&proof).is_some())
        };
        let st = StateDir::open(root).expect("boot 2's open");
        st.set_inc_seam(in_boot(2));
        let began = st.begin_run().expect("boot 2's first start");
        assert_eq!((began.ordered, began.full), (2, 0), "by the barrier");
        assert!(
            !proved(&st),
            "an OPEN the barrier ordered is not proved by its start"
        );
        if inc_fails {
            st.refuse_writes_of(Some("inc"));
            assert!(st.take_incarnation(0).is_err(), "the write of inc refused");
            st.refuse_writes_of(None);
        } else {
            st.take_incarnation(0).expect("boot 2's first incarnation");
        }
        assert_eq!(
            proved(&st),
            !inc_fails,
            "proved once the incarnation's full flush of inc returned, and only then"
        );
        drop(st);
        let next = StateDir::open(root).expect("its successor's open");
        next.set_inc_seam(in_boot(2));
        let began = next.begin_run().expect("the successor's start");
        if inc_fails {
            assert_eq!(
                (began.ordered, began.full, began.proven),
                (0, 2, false),
                "CONTROL: no proof, OPEN written in full over this boot's open"
            );
        } else {
            assert_eq!(
                (began.ordered, began.full, began.proven),
                (0, 0, true),
                "the proved OPEN: nothing written"
            );
        }
    }
}

#[test]
fn after_a_clean_stop_damage_is_refused_across_a_reboot() {
    use aterm_durable::Flush;
    for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::Unsupported] {
        for (reopen, next_boot) in [(false, 1), (true, 1), (false, 2), (true, 2)] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("clean-open-error")
                .suffix(".noindex")
                .tempdir_in("/tmp")
                .expect("scratch");
            let st = StateDir::open(dir.path()).expect("open");
            st.set_inc_seam(in_boot(1));
            st.begin_run().expect("initial OPEN");
            st.take_incarnation(0).expect("a lease in this boot");
            st.end_run_clean().expect("prior clean stop");
            st.set_inc_seam(in_boot(next_boot));
            let tier = if next_boot == 1 {
                Flush::Durable
            } else {
                Flush::Ordered
            };
            st.refuse_directory_flush(Some((dir.path(), tier, kind)));
            assert_eq!(
                st.begin_run().expect_err("OPEN publication refused").kind(),
                kind
            );
            assert!(
                !st.last_run().unwrap().unwrap().clean,
                "rename already visible"
            );
            let st = if reopen {
                drop(st);
                let st = StateDir::open(dir.path()).expect("reopen after failed publication");
                st.set_inc_seam(in_boot(next_boot));
                st
            } else {
                st
            };
            st.refuse_directory_flush(Some((dir.path(), Flush::Durable, kind)));
            assert_eq!(
                st.begin_run()
                    .expect_err("visible OPEN is not admission")
                    .kind(),
                kind
            );
            st.refuse_directory_flush(None);
            let retried = st.begin_run().expect("successful OPEN retry");
            assert_eq!((retried.ordered, retried.full), (0, 2));
            let before = st.ops();
            st.take_incarnation(0)
                .expect("the existing same-boot lease");
            assert_eq!(
                (st.ops() - before).fsyncs,
                if next_boot == 1 { 0 } else { 2 },
                "only a new boot's incarnation supplies a later full flush"
            );
            let before = st.ops();
            assert_eq!(
                st.begin_run().expect("same open reconnect"),
                Flushed::default()
            );
            assert_eq!(st.ops() - before, FileOps::default());
            let replacement = st.path("run-replacement");
            std::fs::write(&replacement, std::fs::read(st.path("run")).unwrap()).unwrap();
            std::fs::rename(replacement, st.path("run")).unwrap();
            assert_eq!(
                st.begin_run().expect("replacement needs a new fence").full,
                2
            );
        }
    }
    for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::Unsupported] {
        for final_root in [false, true] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("clean-dir-error")
                .suffix(".noindex")
                .tempdir_in("/tmp")
                .expect("scratch");
            let st = StateDir::open(dir.path()).expect("open");
            st.set_inc_seam(in_boot(1));
            st.begin_run().expect("an open run");
            let entry = in_flight_entry(7);
            st.set_in_flight(&entry).expect("an entry");
            st.set_post_pin("s-a", 1, &entry.pin)
                .expect("a reservation");
            st.set_receipt_pin("s-a", 4, &entry.pin)
                .expect("a receipt pin");
            let pending = st.unflushed.lock().unwrap().clone();
            assert_eq!(pending.len(), 2, "the journal and the receipt's pin");
            let open = std::fs::read(st.path("run")).expect("the open marker");
            let (path, tier) = if final_root {
                (st.root.clone(), Flush::Durable)
            } else {
                (st.path(journal::DIR), Flush::Ordered)
            };
            *st.dir_flush_refusal.lock().unwrap() = Some((path, tier, kind));
            assert_eq!(
                st.end_run_clean()
                    .expect_err("a required directory barrier")
                    .kind(),
                kind
            );
            assert_eq!(
                *st.unflushed.lock().unwrap(),
                pending,
                "a failed stop retains every pending file for retry"
            );
            if !final_root {
                assert_eq!(std::fs::read(st.path("run")).unwrap(), open);
                assert!(!st.last_run().unwrap().unwrap().clean);
            }
            *st.dir_flush_refusal.lock().unwrap() = None;
            let flushed = st.end_run_clean().expect("a complete retry");
            assert_eq!(flushed.files, pending.len() as u64);
            assert_eq!(flushed.dirs, 2);
            assert_eq!(flushed.full, 1);
            assert!(st.unflushed.lock().unwrap().is_empty());
            assert!(st.last_run().unwrap().unwrap().clean);
        }
    }
    for clean in [true, false] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("clean")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let killed = StateDir::open(root).expect("open");
        killed.set_inc_seam(in_boot(1));
        killed.begin_run().expect("the first run");
        let entry = in_flight_entry(7);
        killed.set_in_flight(&entry).expect("an entry");
        killed
            .set_post_pin("s-a", 1, &entry.pin)
            .expect("a reservation");
        drop(killed);
        let st = StateDir::open(root).expect("its successor, in the same boot");
        st.set_inc_seam(in_boot(1));
        st.begin_run().expect("the second run");
        st.set_receipt_pin("s-a", 4, &entry.pin)
            .expect("a receipt pin");
        if clean {
            st.refuse_writes_of(Some(journal::FILE));
            assert!(
                st.end_run_clean().is_err(),
                "a controlled failed flush attempt"
            );
            assert_eq!(
                st.last_run().expect("run reads").map(|run| run.clean),
                Some(false)
            );
            st.refuse_writes_of(None);
            let before = st.ops();
            let flushed = st.end_run_clean().expect("a clean stop");
            let ops = st.ops() - before;
            assert_eq!(
                (ops.replaces, ops.renames),
                (1, 1),
                "the two files of the two runs flushed where they are: only `run` written"
            );
            assert_eq!((ops.barriers, ops.fsyncs), (2 + 2 + 1, 1));
            assert_eq!(
                flushed,
                Flushed {
                    files: 2,
                    dirs: 2,
                    ordered: 5,
                    full: 1,
                    proven: false,
                }
            );
            assert_eq!(
                st.last_run().expect("run reads").map(|run| run.clean),
                Some(true)
            );
        }
        std::fs::create_dir_all(root.join("sent")).expect("an older build's");
        std::fs::write(root.join("sent/s-a.1"), b"7 key identity\n").expect("a bug's damage");
        let journal = root.join(journal::FILE);
        let mut damaged = std::fs::read(&journal).expect("the journal");
        damaged[20] ^= 0x01;
        std::fs::write(&journal, &damaged).expect("a bug's damage");
        st.set_inc_seam(in_boot(2));
        let moved = st.set_aside_torn().expect("the sweep");
        if clean {
            assert!(moved.is_empty(), "nothing a clean stop left can be torn");
            let next = StateDir::open(root).expect("the next boot's bridge");
            next.set_inc_seam(in_boot(2));
            assert!(next.post_pin("s-a", 1).is_err(), "refused, fail-closed");
            std::fs::remove_file(root.join("sent/s-a.1")).expect("one repaired");
            let next = StateDir::open(root).expect("the next boot's bridge");
            next.set_inc_seam(in_boot(2));
            assert!(
                next.post_pin("s-a", 1).is_err(),
                "a damaged journal refused too"
            );
            assert_eq!(std::fs::read(&journal).expect("kept"), damaged);
        } else {
            assert_eq!(
                moved.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(),
                ["sent/s-a.1", journal::FILE],
                "CONTROL: a run that never closed is judged unclean"
            );
        }
    }
}

#[test]
fn a_flush_in_place_passes_over_what_is_gone_and_refuses_a_link() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("in-place")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let elsewhere = aterm_tempfile::Builder::new()
        .prefix("in-place-target")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let st = StateDir::open(dir.path()).expect("open");
    st.set_inc_seam(in_boot(1));
    st.write("keys/s-a", "k\n").expect("a key file");
    let flush = aterm_durable::Flush::Ordered;
    let before = st.ops();
    assert!(st
        .flush_in_place("keys/s-a", flush)
        .expect("a regular file"));
    assert_eq!((st.ops() - before).barriers, 1, "flushed where it is");
    let before = st.ops();
    assert!(!st.flush_in_place("keys/s-b", flush).expect("gone"));
    std::fs::create_dir(st.path("keys/s-c")).expect("a planted directory");
    assert!(!st.flush_in_place("keys/s-c", flush).expect("a directory"));
    let target = elsewhere.path().join("outside");
    std::fs::write(&target, b"outside").expect("what a link would name");
    std::os::unix::fs::symlink(&target, st.path("keys/s-d")).expect("a planted link");
    assert!(
        st.flush_in_place("keys/s-d", flush).is_err(),
        "a link is the open's error, never passed over as if gone"
    );
    assert_eq!(
        (st.ops() - before).barriers,
        0,
        "nothing passed over or refused is flushed"
    );
}

#[test]
fn a_whole_in_flight_entry_of_an_earlier_boot_is_retired_and_within_a_boot_kept() {
    for (case, boot, clean) in [
        ("an unclean stop, then a reboot", 2, false),
        ("a kill in the same boot", 1, false),
        ("a clean stop, then a reboot", 2, true),
    ] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("retire")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(in_boot(1));
        st.begin_run().expect("boot 1's run");
        let (seven, nine) = (in_flight_entry(7), in_flight_entry(9));
        st.set_in_flight(&seven).expect("a whole entry");
        st.set_in_flight(&nine).expect("another");
        let on_disk = format!("{}\n", seven.render()).into_bytes();
        if clean {
            st.end_run_clean().expect("a clean stop");
        }
        let entry = in_flight_entry(8).render();
        let cut = &entry.as_bytes()[..entry.find("/s-a/").expect("the face") + 3];
        std::fs::create_dir_all(root.join("inflight")).expect("an older build's");
        std::fs::write(root.join("inflight/8"), cut).expect("a cut entry");
        st.set_inc_seam(in_boot(boot));
        let retired = st
            .retire_in_flight_of_an_earlier_boot()
            .expect("the retirement");
        if boot == 1 || clean {
            assert!(retired.is_empty(), "{case}: nothing is retired");
            assert!(!root.join(QUARANTINE).exists(), "{case}: nothing moved");
            assert_eq!(
                st.in_flight().expect("the journal reads"),
                vec![(7, seven.clone()), (9, nine.clone())],
                "{case}: both stay, for the attach to publish again"
            );
            continue;
        }
        assert_eq!(
            retired,
            [7, 9]
                .map(|seq| SetAside {
                    from: format!("inflight/{seq}"),
                    to: format!("{QUARANTINE}/{RETIRED_PREFIX}inflight.{seq}"),
                })
                .to_vec(),
            "every whole entry, and only those"
        );
        assert_eq!(
            std::fs::read(root.join("quarantine/retired.inflight.7")).expect("kept"),
            on_disk,
            "the bytes its file would have held, never deleted"
        );
        assert!(
            st.in_flight().expect("the journal reads").is_empty(),
            "the journal's records dropped"
        );
        let next = || {
            let next = StateDir::open(root).expect("the next bridge");
            next.set_inc_seam(in_boot(boot));
            next
        };
        assert_eq!(
            next()
                .in_flight()
                .expect_err("the cut one, the torn sweep's")
                .kind(),
            io::ErrorKind::InvalidData
        );
        let torn = st.set_aside_torn().expect("the torn sweep");
        assert_eq!(
            torn.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(),
            ["inflight/8"]
        );
        assert!(
            next().in_flight().expect("the journal reads").is_empty(),
            "nothing left for the attach to publish again"
        );
        assert!(
            st.retire_in_flight_of_an_earlier_boot()
                .expect("again")
                .is_empty(),
            "idempotent"
        );
        st.set_in_flight(&seven).expect("the same record again");
        assert!(
            st.retire_in_flight_of_an_earlier_boot()
                .expect("again")
                .is_empty(),
            "one record, one copy"
        );
        assert!(
            st.in_flight().expect("reads").is_empty(),
            "and it is dropped"
        );
        let other = InFlight {
            body: b"v=1 t=6 text=another".to_vec(),
            ..in_flight_entry(7)
        };
        st.set_in_flight(&other)
            .expect("another record at a retired name");
        let again = st.retire_in_flight_of_an_earlier_boot().expect("again");
        assert_eq!(
            again.iter().map(|m| m.to.as_str()).collect::<Vec<_>>(),
            [format!("{QUARANTINE}/{RETIRED_PREFIX}inflight.7.1")]
        );
        assert_eq!(
            std::fs::read(root.join("quarantine/retired.inflight.7")).expect("kept"),
            on_disk,
            "the first one is not overwritten"
        );
    }
}

/// A TORN FILE AND THE RECORD THE JOURNAL HOLDS AT ITS SEQUENCE KEEP ONE COPY EACH: a stop's
/// hand-off cut before its unlink leaves a record in the journal and its `inflight/<seq>`
/// torn. The recovery copies the file, the disk refuses the unlink, and it goes on to retire
/// the record, a copy under the same source's name; tried again, the sweep reuses the file's
/// copy. CONTROL ([`crate::sim::world::Mutant::OneQuarantineMemoryPerSource`], as before):
/// the record's copy ends the memory of the file's, so the next try makes a second copy of
/// the file (the sim's found row for that mutant).
#[test]
fn a_torn_file_and_the_record_at_its_sequence_keep_one_copy_each() {
    use std::os::unix::fs::PermissionsExt as _;
    struct RestoreMode(PathBuf, std::fs::Permissions);
    impl Drop for RestoreMode {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, self.1.clone());
        }
    }
    for (one_memory, held) in [
        (false, &["inflight.7", "retired.inflight.7"][..]),
        (
            true,
            &["inflight.7", "inflight.7.1", "retired.inflight.7"][..],
        ),
    ] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("torn-and-record")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.one_quarantine_memory_per_source(one_memory);
        st.set_inc_seam(in_boot(1));
        st.begin_run().expect("boot 1's run");
        st.set_in_flight(&in_flight_entry(7))
            .expect("in the journal");
        std::fs::create_dir_all(root.join("inflight")).expect("the folder");
        std::fs::write(root.join("inflight/7"), b"torn 7").expect("and torn in a file");
        st.set_inc_seam(in_boot(2));
        let inflight = root.join("inflight");
        let mode = |m: u32| {
            std::fs::set_permissions(&inflight, std::fs::Permissions::from_mode(m)).expect("chmod");
        };
        let _restore = RestoreMode(
            inflight.clone(),
            std::fs::metadata(&inflight).unwrap().permissions(),
        );
        mode(0o555);
        st.set_aside_torn().expect_err("the unlink fails");
        st.retire_in_flight_of_an_earlier_boot()
            .expect("the record retired");
        mode(0o755);
        st.set_aside_torn().expect("the sweep, its folder writable");
        assert!(!root.join("inflight/7").exists(), "moved");
        let mut copies: Vec<String> = std::fs::read_dir(root.join(QUARANTINE))
            .expect("the quarantine")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.'))
            .collect();
        copies.sort();
        assert_eq!(copies, held, "one_memory={one_memory}");
    }
}

/// ONE RECORD, ONE COPY: a record a stop's hand-off left both in an older build's file and
/// in the journal (a cut between the file and the journal's unlink) is retired once, its
/// one copy in the quarantine, and dropped from the journal. CONTROL: the record alone in
/// the journal is retired too.
#[test]
fn a_record_in_both_a_file_and_the_journal_is_retired_once() {
    for in_file in [true, false] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("retire-both")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(in_boot(1));
        st.begin_run().expect("boot 1's run");
        let seven = in_flight_entry(7);
        st.set_in_flight(&seven).expect("in the journal");
        if in_file {
            st.write("inflight/7", &seven.render())
                .expect("and in a file");
        }
        st.set_inc_seam(in_boot(2));
        let retired = st
            .retire_in_flight_of_an_earlier_boot()
            .expect("the retirement");
        assert_eq!(
            retired.iter().map(|m| m.to.as_str()).collect::<Vec<_>>(),
            [format!("{QUARANTINE}/{RETIRED_PREFIX}inflight.7")],
            "in_file={in_file}: one copy"
        );
        let copies: Vec<String> = std::fs::read_dir(root.join(QUARANTINE))
            .expect("the quarantine")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.'))
            .collect();
        assert_eq!(copies, ["retired.inflight.7"], "in_file={in_file}");
        assert!(
            StateDir::open(root)
                .expect("reopen")
                .in_flight()
                .unwrap()
                .is_empty(),
            "in_file={in_file}: nothing left to publish again"
        );
    }
}

/// **AN ATTACH OF THIS BOOT ON RECORD STOPS THE RETIREMENT** (finding F2): `run` still names
/// boot 1's unclean stop, as a start of boot 2 whose `run` the disk refused leaves it, and an
/// attach of boot 2 took an incarnation: its entries may be live sessions', and nothing is
/// retired. On the boot volume and off it (item RZ2), the lease naming the kernel's boot on
/// either. NEGATIVE CONTROLS: a lease of boot 1 retires the entry, and so does boot 2's
/// attach off the boot volume by an older build, whose lease names no boot there
/// (`lease_names_no_boot_off_the_boot_volume`).
#[test]
fn an_attach_of_this_boot_on_record_stops_the_retirement() {
    for (case, off_volume, attached_in_boot_2, legacy, kept) in [
        ("a lease of boot 2", false, true, false, true),
        ("a lease of boot 1", false, false, false, false),
        (
            "off the boot volume, a lease of boot 2",
            true,
            true,
            false,
            true,
        ),
        (
            "off the boot volume, a lease of boot 1",
            true,
            false,
            false,
            false,
        ),
        (
            "off the boot volume, an older build's lease of boot 2, which names no boot",
            true,
            true,
            true,
            false,
        ),
        (
            "on it, an older build's lease of boot 2, which names it",
            false,
            true,
            true,
            true,
        ),
    ] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("retire-lease")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        let seam = |b| IncSeam {
            off_volume,
            ..in_boot(b)
        };
        st.set_inc_seam(seam(1));
        st.begin_run().expect("boot 1's run");
        st.take_incarnation(0).expect("boot 1's attach");
        st.set_in_flight(&in_flight_entry(7))
            .expect("a whole entry");
        st.set_inc_seam(seam(2));
        st.refuse_writes_of(Some("run"));
        assert_eq!(
            st.begin_run().expect_err("the disk refuses `run`").kind(),
            io::ErrorKind::StorageFull,
            "{case}"
        );
        assert_eq!(
            st.last_run().expect("run reads"),
            Some(LastRun {
                boot: Some(boot_id(1)),
                clean: false
            }),
            "{case}: `run` still names boot 1"
        );
        if attached_in_boot_2 {
            st.lease_names_no_boot_off_the_boot_volume(legacy);
            st.take_incarnation(0).expect("boot 2's attach");
        }
        let retired = st
            .retire_in_flight_of_an_earlier_boot()
            .expect("the retirement");
        if kept {
            assert!(retired.is_empty(), "{case}: nothing is retired");
            assert_eq!(
                st.in_flight().expect("reads").len(),
                1,
                "{case}: the entry stays"
            );
        } else {
            assert_eq!(
                retired.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(),
                ["inflight/7"],
                "{case}: NEGATIVE CONTROL"
            );
        }
    }
}

#[test]
fn a_cut_in_flight_entry_that_still_reads_is_set_aside_after_an_unclean_reboot() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("cut")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = dir.path();
    let st = StateDir::open(root).expect("open");
    st.set_inc_seam(in_boot(1));
    st.take_incarnation(0).expect("the last attach, in boot 1");
    let reply = |seq| InFlight {
        body: b"v=1 t=5 re=42 text=a reply".to_vec(),
        log: astream_broker::LogIdentity::parse(&format!("{}.12", "5a".repeat(16))),
        ..in_flight_entry(seq)
    };
    let whole = reply(7);
    let older_build =
        |entry: &InFlight| st.write(&format!("inflight/{}", entry.pin.seq), &entry.render());
    older_build(&whole).expect("a whole entry");
    for seq in [8, 9, 10] {
        older_build(&reply(seq)).expect("an entry the cut will take");
    }
    let at = |seq: u64, text: &str| match seq {
        8 => text.find("log=").expect("its witness"),
        9 => text.match_indices('\n').nth(1).expect("its face line").0 + 1 + 12,
        _ => text.len() - 2,
    };
    for seq in [8, 9, 10] {
        let path = root.join(format!("inflight/{seq}"));
        let text = std::fs::read_to_string(&path).expect("as written");
        let cut = &text[..at(seq, &text)];
        std::fs::write(&path, cut).expect("the cut");
        let read = InFlight::parse(seq, cut).expect("PRECONDITION: the cut still reads");
        assert_eq!(read.log.is_some(), seq == 10, "{cut:?}");
    }
    st.set_inc_seam(in_boot(2));
    assert!(
        st.set_aside_torn_by_the_reader_alone()
            .expect("a sweep by the reader alone")
            .is_empty(),
        "NEGATIVE CONTROL: the reader takes every cut entry, so a sweep that trusts it alone \
         keeps them, the replies among them with no witness of their log"
    );
    let moved = st.set_aside_torn().expect("the sweep");
    assert_eq!(
        moved.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(),
        ["inflight/10", "inflight/8", "inflight/9"],
        "every cut entry, and not the whole one"
    );
    assert_eq!(st.in_flight().expect("reads"), vec![(7, whole)]);
}

#[test]
fn damage_in_a_boot_nobody_can_name_is_refused_not_set_aside() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("unnamed")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = dir.path();
    let st = StateDir::open(root).expect("open");
    st.set_inc_seam(IncSeam {
        boot: Some(None),
        ..IncSeam::default()
    });
    st.take_incarnation(0).expect("an attach");
    std::fs::create_dir_all(root.join("sent")).expect("sent/");
    std::fs::write(root.join("sent/s-a.1"), b"12 key ident").expect("a damaged reservation");
    assert!(st.set_aside_torn().expect("no sweep").is_empty());
    assert!(st.post_pin("s-a", 1).is_err(), "refused, fail-closed");
}

#[test]
fn a_torn_logpin_is_left_for_the_mirror_to_refuse() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("logpin")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = dir.path();
    let st = StateDir::open(root).expect("open");
    st.set_inc_seam(in_boot(1));
    st.take_incarnation(0).expect("the last attach, in boot 1");
    std::fs::write(root.join("logpin"), b"").expect("torn to nothing");
    st.set_inc_seam(in_boot(2));
    assert!(st.set_aside_torn().expect("the sweep").is_empty());
    assert_eq!(
        st.reconcile_logpin().expect("the reconcile"),
        Reconciled::Kept,
        "no floor on the media: the pin holds the only copies"
    );
    assert_eq!(st.logpin().expect("read").as_deref(), Some(""));
}

fn mirror_pin(key: [u8; 16], total: u64) -> crate::mirror::Pin {
    crate::mirror::Pin {
        inc: 1,
        total,
        key: Some(key),
        win: Vec::new(),
        slots: vec![crate::mirror::PinSlot {
            pseq: 9,
            sid: "s-a".to_string(),
            at: aterm_slog::BusCursor::new(4, 2, 1),
            body: "v=2 t=1 ep=4 seq=2 ms=1 op=open node=t1 k=turn src=screen conf=strong"
                .to_string(),
        }],
    }
}

#[test]
fn a_torn_logpin_with_its_copies_is_set_aside_after_an_unclean_stop_and_kept_otherwise() {
    let key = [0x5a; 16];
    for (world, key_copy, floor_copy, reboot, clean, older) in [
        (
            "an unclean stop, then a reboot",
            true,
            true,
            true,
            false,
            false,
        ),
        ("a kill, in the same boot", true, true, false, false, false),
        ("no floor on the media", true, false, true, false, false),
        ("no key in logkey", false, true, true, false, false),
        ("a clean stop, then a reboot", true, true, true, true, false),
        (
            "an older build's pin over the copies",
            true,
            true,
            true,
            false,
            true,
        ),
    ] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("logpin")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(in_boot(1));
        st.begin_run().expect("the last bridge's run, in boot 1");
        st.take_incarnation(0).expect("its attach");
        if key_copy {
            st.set_logkey(&key).expect("the key's copy");
        }
        if floor_copy {
            assert_eq!(st.raise_logfloor(5000).expect("the floor"), 5000);
        }
        st.set_logpin(&mirror_pin(key, 3000).render())
            .expect("the first pin");
        let pin = mirror_pin(key, 4000);
        st.set_logpin(&pin.render()).expect("the pin");
        if clean {
            let before = st.ops();
            st.end_run_clean().expect("a clean stop");
            let ops = st.ops() - before;
            assert_eq!(
                (ops.replaces, ops.barriers, ops.fsyncs),
                (1, 3, 1),
                "{world}: the pin flushed where it is and the root ordered after it, then \
                 `run`'s bytes ordered and the root flushed in full"
            );
        }
        let mut text = pin.render();
        if older {
            text = text.replacen("logpin v=2", "logpin v=1", 1);
        }
        let cut = &text.as_bytes()[..text.len() - 7];
        std::fs::write(root.join("logpin"), cut).expect("cut short");
        if reboot {
            st.set_inc_seam(in_boot(2));
        }
        let done = st.reconcile_logpin().expect("the reconcile");
        if world == "an unclean stop, then a reboot" {
            assert_eq!(
                done,
                Reconciled::SetAside(SetAside {
                    from: "logpin".to_string(),
                    to: format!("{QUARANTINE}/logpin"),
                }),
                "{world}"
            );
            assert_eq!(
                std::fs::read(root.join("quarantine/logpin")).expect("kept"),
                cut,
                "its exact bytes, never deleted"
            );
            let placed = st.logpin().expect("reads").expect("a pin in its place");
            assert!(
                crate::mirror::Pin::by_this_build(placed.as_bytes()),
                "of the version an older build refuses: {placed:?}"
            );
            let from_the_copies = crate::mirror::Pin {
                key: Some(key),
                total: 5000,
                ..crate::mirror::Pin::default()
            };
            assert_eq!(
                crate::mirror::Pin::parse(&placed),
                Ok(from_the_copies.clone()),
                "no batch, logkey's key, logfloor's total"
            );
            assert!(
                !st.unflushed
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(LOGPIN),
                "written durably: no cut brings back the absence"
            );
            assert_eq!(
                st.logkey().expect("reads"),
                Some(key),
                "the key's copy stays"
            );
            assert_eq!(
                st.logfloor().expect("reads"),
                Some(5000),
                "so does the floor"
            );
            assert_eq!(st.reconcile_logpin().expect("again"), Reconciled::Kept);
            std::fs::remove_file(root.join("logpin")).expect("gone");
            assert_eq!(
                st.reconcile_logpin().expect("the reconcile"),
                Reconciled::Restored
            );
            assert_eq!(
                st.logpin()
                    .expect("reads")
                    .map(|t| crate::mirror::Pin::parse(&t)),
                Some(Ok(from_the_copies)),
                "written again"
            );
            st.set_logpin(&mirror_pin(key, 100).render())
                .expect("an older pin");
            assert_eq!(
                st.reconcile_logpin().expect("the reconcile"),
                Reconciled::Raised {
                    from: 100,
                    to: 5000
                }
            );
            let raised = crate::mirror::Pin::parse(&st.logpin().expect("reads").expect("a pin"))
                .expect("whole");
            assert_eq!(raised, mirror_pin(key, 5000), "its batch and key kept");
            assert_eq!(st.reconcile_logpin().expect("again"), Reconciled::Kept);
        } else {
            assert_eq!(done, Reconciled::Kept, "{world}");
            assert_eq!(
                st.logpin()
                    .expect("reads")
                    .map(String::into_bytes)
                    .as_deref(),
                Some(cut),
                "{world}: kept as it is, for the mirror to refuse"
            );
            assert!(!root.join(QUARANTINE).exists(), "{world}: nothing moved");
        }
    }
}

#[test]
fn the_first_pin_of_this_build_is_durable_and_the_rest_atomic() {
    let older = mirror_pin([7; 16], 10)
        .render()
        .replacen("logpin v=2", "logpin v=1", 1);
    for (world, before) in [
        ("no pin", None),
        ("an older build's pin", Some(older.as_bytes())),
        ("damage that names no version", Some(&b"logpin v="[..])),
        ("this build's pin, torn", Some(&b"logpin v=2 inc=1 n"[..])),
    ] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("logpin")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        if let Some(bytes) = before {
            std::fs::write(root.join("logpin"), bytes).expect("what is there");
        }
        let ops = st.ops();
        st.set_logpin(&mirror_pin([7; 16], 20).render())
            .expect("the pin");
        assert_eq!((st.ops() - ops).fsyncs, 2, "{world}");
        let ops = st.ops();
        st.set_logpin(&mirror_pin([7; 16], 30).render())
            .expect("the next");
        assert_eq!((st.ops() - ops).fsyncs, 0, "{world}: the next is atomic");
    }

    let dir = aterm_tempfile::Builder::new()
        .prefix("logpin")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = dir.path();
    let pin = mirror_pin([7; 16], 40).render();
    let st = StateDir::open(root).expect("open");
    st.set_inc_seam(IncSeam {
        kill_at: Some(IncStep::Renamed),
        ..Default::default()
    });
    assert!(
        st.set_logpin(&pin).is_err(),
        "the first flush failed after rename"
    );
    assert!(crate::mirror::Pin::by_this_build(
        &std::fs::read(root.join(LOGPIN)).unwrap()
    ));
    st.set_inc_seam(IncSeam::default());
    let ops = st.ops();
    st.set_logpin(&pin).expect("retried");
    assert_eq!((st.ops() - ops).fsyncs, 2, "the retry is still durable");
    let ops = st.ops();
    st.ensure_logpin_durable(|| panic!("a known fence needs no render or rewrite"))
        .expect("already admitted");
    assert_eq!((st.ops() - ops).replaces, 0);

    let st = StateDir::open(root).expect("reopen");
    let ops = st.ops();
    st.ensure_logpin_durable(|| pin.clone())
        .expect("reestablished after reopen");
    assert_eq!((st.ops() - ops).fsyncs, 2);
    for change in ["removed", "replaced", "legacy header"] {
        let path = root.join(LOGPIN);
        match change {
            "removed" => std::fs::remove_file(&path).unwrap(),
            "replaced" => {
                let staged = root.join("other-pin");
                std::fs::write(&staged, &pin).unwrap();
                std::fs::rename(staged, &path).unwrap();
            }
            _ => std::fs::write(&path, pin.replacen("v=2", "v=1", 1)).unwrap(),
        }
        let ops = st.ops();
        st.ensure_logpin_durable(|| pin.clone()).expect(change);
        assert_eq!(
            (st.ops() - ops).fsyncs,
            2,
            "{change}: admission invalidated"
        );
        let ops = st.ops();
        st.set_logpin(&pin).expect("the next own replacement");
        assert_eq!(
            (st.ops() - ops).fsyncs,
            0,
            "{change}: later own writes stay atomic"
        );
    }
    st.refuse_writes_of(Some(LOGPIN));
    assert!(
        st.set_logpin(&pin).is_err(),
        "an ordinary atomic write failed"
    );
    st.refuse_writes_of(None);
    let ops = st.ops();
    st.ensure_logpin_durable(|| pin)
        .expect("readmitted after the error");
    assert_eq!(
        (st.ops() - ops).fsyncs,
        2,
        "any write error revokes admission"
    );
}

/// THE PIN'S FENCE BY BARRIERS AND A SETTLE (critic item F07): `order_logpin` writes the pin by
/// two barriers (the temp's and the directory's) and no full flush, and is no fence until
/// `logpin_settled` makes it one; asked again before then it writes nothing, and once it is the
/// fence it is held, with no render, as `ensure_logpin_durable`'s. The controls: a pin
/// replaced between the order and the settle (by the open's own durable write, or by
/// another file renamed over the name) is not made the fence by the settle, and a settle with
/// nothing ordered fences nothing.
#[test]
fn an_ordered_pin_is_the_fence_once_its_settle_is_answered() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("logpin-ordered")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = dir.path();
    let pin = mirror_pin([7; 16], 40).render();
    let st = StateDir::open(root).expect("open");
    assert!(!st.logpin_settled(), "nothing ordered, nothing settled");
    let ops = st.ops();
    assert!(!st.order_logpin(|| pin.clone()).expect("ordered"));
    let made = st.ops() - ops;
    assert_eq!(
        (made.replaces, made.barriers, made.fsyncs),
        (1, 2, 0),
        "the temp's barrier and the directory's, no full flush"
    );
    assert!(!st.logpin_fence_held(), "no fence before the settle");
    let ops = st.ops();
    assert!(!st
        .order_logpin(|| panic!("an ordered pin is not written again"))
        .expect("still ordered"));
    assert_eq!((st.ops() - ops).replaces, 0);
    assert!(st.logpin_settled(), "the settle makes the fence");
    assert!(st.logpin_fence_held());
    assert!(st
        .order_logpin(|| panic!("a held fence needs no render"))
        .expect("held"));
    let ops = st.ops();
    st.set_logpin(&mirror_pin([7; 16], 50).render())
        .expect("the next");
    assert_eq!((st.ops() - ops).fsyncs, 0, "the next pin is atomic");

    // The controls, each on a fresh open of the dir.
    for change in ["durable", "renamed over"] {
        let st = StateDir::open(root).expect("reopen");
        assert!(!st.order_logpin(|| pin.clone()).expect(change));
        match change {
            "durable" => st.ensure_logpin_durable(|| pin.clone()).expect(change),
            _ => {
                let staged = root.join("other-pin");
                std::fs::write(&staged, &pin).unwrap();
                std::fs::rename(staged, root.join(LOGPIN)).unwrap();
            }
        }
        let held = st.logpin_settled();
        assert_eq!(
            held,
            change == "durable",
            "{change}: only the open's own fence stands, never a pin it did not order"
        );
        assert_eq!(st.logpin_fence_held(), held, "{change}");
    }
}

#[test]
fn the_mirrors_copies_read_back_and_the_floor_is_only_raised() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("copies")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = dir.path();
    let st = StateDir::open(root).expect("open");
    assert_eq!(st.logkey().expect("none"), None);
    assert_eq!(st.logfloor().expect("none"), None);
    st.set_logkey(&[3; 16]).expect("the key");
    assert_eq!(st.logkey().expect("reads"), Some([3; 16]));
    assert_eq!(st.raise_logfloor(700).expect("raised"), 700);
    let before = st.ops();
    assert_eq!(st.raise_logfloor(600).expect("kept"), 700, "never lowered");
    assert_eq!((st.ops() - before).replaces, 0, "nothing written");
    assert_eq!(st.raise_logfloor(900).expect("raised"), 900);
    assert_eq!(st.logfloor().expect("reads"), Some(900));
    st.set_inc_seam(in_boot(1));
    st.begin_run().expect("a run in boot 1");
    std::fs::write(root.join("logpin"), b"logpin v=2 inc=1 n=0 to").expect("a torn pin");
    st.set_inc_seam(in_boot(2));
    for (name, damage) in [("logfloor", &b"90"[..]), ("logkey", &b"03030303\n"[..])] {
        let good = std::fs::read(root.join(name)).expect("the copy");
        std::fs::write(root.join(name), damage).expect("damaged");
        assert_eq!(
            if name == "logkey" {
                st.logkey().map(|_| ()).expect_err("refused").kind()
            } else {
                st.logfloor().map(|_| ()).expect_err("refused").kind()
            },
            io::ErrorKind::InvalidData,
            "{name}"
        );
        assert!(
            st.reconcile_logpin().is_err(),
            "{name}: no move on a copy that does not read"
        );
        assert!(root.join("logpin").exists(), "{name}: the pin stays");
        std::fs::write(root.join(name), good).expect("repaired");
    }
    assert!(matches!(
        st.reconcile_logpin().expect("the reconcile"),
        Reconciled::SetAside(_)
    ));
}
