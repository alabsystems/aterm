// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::unix::fs::PermissionsExt as _;

use super::*;
use crate::durability::{self, Durability};

const SENTINEL: &[u8] = b"QZREST-sentinel-of-the-log";

struct Scratch {
    dir: aterm_tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        Self {
            dir: aterm_tempfile::Builder::new()
                .prefix("ar")
                .tempdir_in("/tmp")
                .expect("scratch"),
        }
    }

    fn log(&self) -> PathBuf {
        self.dir.path().join("bus.log")
    }

    fn sock(&self) -> PathBuf {
        self.dir.path().join("b.sock")
    }

    fn key(&self) -> PathBuf {
        key_path(&self.log())
    }

    /// Plant `key` as the log's, what `mint` leaves.
    fn plant_key(&self, key: &[u8; KEY_LEN]) {
        aterm_uds::private_file::write_private(self.dir.path(), REST_KEY, key).expect("key");
    }

    /// Serve the log under `key` and publish `n` records carrying the sentinel, then close it.
    fn fill(&self, key: Option<[u8; KEY_LEN]>, n: u64) -> String {
        let opened = durability::open_escalating(
            &self.log(),
            None,
            Durability::Ordered,
            Some([7; 16]),
            Vec::new(),
            key,
        )
        .expect("open");
        // As `aterm link broker` does at every open: so a real broker's `.held` is here too.
        crate::broker_rules::retain(&opened.broker).expect("the rules");
        let identity = opened.broker.log_identity().to_string();
        let served = opened.broker.serve(self.sock()).expect("serve");
        let mut client = astream_broker::Client::connect(self.sock()).expect("connect");
        for seq in 1..=n {
            let mut body = SENTINEL.to_vec();
            body.extend_from_slice(format!("-{seq}").as_bytes());
            client.publish(9, seq, "/o/a", &body).expect("acked");
        }
        drop(client);
        drop(served);
        drop(opened);
        identity
    }

    fn holds_sentinel(&self, suffix: &str) -> bool {
        let mut name = self.log().into_os_string();
        name.push(suffix);
        std::fs::read(&name).is_ok_and(|bytes| bytes.windows(SENTINEL.len()).any(|w| w == SENTINEL))
    }

    /// Every record's body in the log, read under `key`.
    fn bodies(&self, key: [u8; KEY_LEN]) -> Vec<Vec<u8>> {
        let log = astream_broker::BrokerLog::open_encrypted_bound(self.log(), key).expect("opens");
        log.read_from(log.base())
            .iter()
            .map(|r| r.body.clone())
            .collect()
    }

    fn snapshot(&self) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(self.dir.path())
            .expect("list")
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let name = entry.file_name().into_string().ok()?;
                let bytes = std::fs::read(entry.path()).ok()?;
                (name != "bus.log.health").then_some((name, bytes))
            })
            .collect();
        files.sort();
        files
    }

    /// [`Self::snapshot`] but for `<log>.id`: an open that closes again rewrites which tier and
    /// boot last served the log. Its identity is in there too, and each test checks that apart.
    fn but_for_the_last_run(&self) -> Vec<(String, Vec<u8>)> {
        let mut files = self.snapshot();
        files.retain(|(name, _)| name != "bus.log.id");
        files
    }
}

const KEY: [u8; KEY_LEN] = *b"at-rest-test-key-of-32-raw-bytes";
const OTHER: [u8; KEY_LEN] = *b"another-at-rest-key-of-32-bytes!";

/// A sealed log with three records and its key beside it.
fn sealed() -> Scratch {
    let s = Scratch::new();
    s.plant_key(&KEY);
    let prepared = prepare(&s.log(), Backups::INERT).expect("prepared");
    assert!(prepared.key.is_some());
    s.fill(prepared.key.as_ref().map(Key::bytes), 3);
    assert!(is_sealed(&s.log()));
    s
}

/// NEGATIVE CONTROL: the keyless arm finds the sentinel in the log, the keyed arm in none of it.
#[test]
fn a_log_beside_a_key_holds_no_sentinel_and_a_keyless_one_does() {
    let plain = Scratch::new();
    plain.fill(None, 3);
    assert!(plain.holds_sentinel(""), "the control holds the sentinel");
    assert_eq!(word(&plain.log()), "plaintext");

    let sealed = sealed();
    assert!(!sealed.holds_sentinel(""));
    assert_eq!(word(&sealed.log()), "sealed");
    assert_eq!(sealed.bodies(KEY).len(), 3, "and it reads under its key");
}

/// No key and no fabric root (a broker run by hand, a test's): plaintext, nothing minted.
#[test]
fn a_log_with_no_mint_secret_beside_it_gets_no_key() {
    let s = Scratch::new();
    let prepared = prepare(&s.log(), Backups::INERT).expect("prepared");
    assert!(prepared.key.is_none());
    assert!(prepared.notes.is_empty(), "{:?}", prepared.notes);
    assert!(!s.key().exists());
}

/// A fabric root mints its key once: 0600, and the next start reads the same one.
#[test]
fn a_fabric_root_mints_its_key_once() {
    let s = Scratch::new();
    aterm_uds::private_file::write_private(s.dir.path(), crate::enable::MINT_SECRET, &[1; KEY_LEN])
        .expect("secret");
    let first = prepare(&s.log(), Backups::INERT).expect("prepared");
    let key = first.key.as_ref().expect("minted").bytes();
    assert!(
        first.notes.iter().any(|n| n.contains("minted")),
        "{:?}",
        first.notes
    );
    let mode = std::fs::metadata(s.key())
        .expect("key")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let second = prepare(&s.log(), Backups::INERT).expect("prepared");
    assert_eq!(second.key.as_ref().expect("kept").bytes(), key);
    assert!(second.notes.is_empty(), "{:?}", second.notes);
}

/// A plaintext log beside a new key seals once at a start: every record at its place, its
/// identity kept, the sentinel gone, and the next start has nothing to do.
#[test]
fn an_existing_plaintext_log_seals_once_record_for_record() {
    let s = Scratch::new();
    let identity = s.fill(None, 5);
    assert!(s.holds_sentinel(""), "plaintext before");
    s.plant_key(&KEY);

    let prepared = prepare(&s.log(), Backups::INERT).expect("prepared");
    assert!(
        prepared.notes.iter().any(|n| n.contains("sealed at rest")),
        "the start logs it: {:?}",
        prepared.notes
    );
    assert!(is_sealed(&s.log()));
    assert!(!s.holds_sentinel(""), "sealed after");
    let bodies = s.bodies(KEY);
    assert_eq!(bodies.len(), 5);
    assert!(bodies[0].starts_with(SENTINEL));
    let reopened = durability::open_escalating(
        &s.log(),
        None,
        Durability::Ordered,
        Some([7; 16]),
        Vec::new(),
        prepared.key.as_ref().map(Key::bytes),
    )
    .expect("keyed and bound");
    assert_eq!(reopened.broker.log_identity().to_string(), identity);
    drop(reopened);

    let again = prepare(&s.log(), Backups::INERT).expect("prepared");
    assert!(again.key.is_some());
    assert!(again.notes.is_empty(), "sealed once: {:?}", again.notes);
}

/// A sealed log refuses a key that is missing or damaged, every way it can be, names the key and
/// the way out, and nothing beside it changes. With the key back it opens.
#[test]
fn a_sealed_log_refuses_a_missing_or_damaged_key_and_changes_nothing() {
    let s = sealed();
    let key = s.key();
    let before = {
        let mut all = s.snapshot();
        all.retain(|(name, _)| name != REST_KEY);
        all
    };
    let undamaged = std::fs::read(&key).expect("the key");
    let write = |bytes: &[u8]| {
        let _ = std::fs::remove_file(&key);
        aterm_uds::private_file::write_private(s.dir.path(), REST_KEY, bytes).expect("write");
    };
    type Damage<'a> = Box<dyn Fn() + 'a>;
    let damages: Vec<(&str, Damage<'_>)> = vec![
        (
            "missing",
            Box::new(|| std::fs::remove_file(&key).expect("remove")),
        ),
        ("short", Box::new(|| write(&[1; 16]))),
        ("long", Box::new(|| write(&[1; KEY_LEN + 1]))),
        (
            "open to others",
            Box::new(|| {
                write(&KEY);
                std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644))
                    .expect("chmod");
            }),
        ),
        (
            "a link",
            Box::new(|| {
                let _ = std::fs::remove_file(&key);
                let beside = s.dir.path().join("real");
                std::fs::write(&beside, KEY).expect("real");
                std::fs::set_permissions(&beside, std::fs::Permissions::from_mode(0o600))
                    .expect("chmod");
                std::os::unix::fs::symlink(&beside, &key).expect("link");
            }),
        ),
    ];
    for (what, damage) in damages {
        damage();
        let refused = prepare(&s.log(), Backups::INERT).expect_err(what);
        assert!(
            refused.why.contains("sealed at rest") && refused.why.contains(&*key.to_string_lossy()),
            "{what}: {refused:?}"
        );
        assert!(
            refused.remedy.contains(&*key.to_string_lossy())
                && refused.remedy.contains("--retire-log"),
            "{what}: {refused:?}"
        );
        let mut after = s.snapshot();
        after.retain(|(name, _)| name != REST_KEY && name != "real");
        assert_eq!(after, before, "{what}: nothing beside the key changes");
        let damaged = || {
            use std::os::unix::fs::MetadataExt as _;
            std::fs::symlink_metadata(&key)
                .ok()
                .map(|m| (m.ino(), m.len(), m.mode()))
        };
        let as_damaged = damaged();
        assert!(
            ensure_key(&s.log(), Backups::INERT, false).is_err(),
            "{what}"
        );
        assert_eq!(
            damaged(),
            as_damaged,
            "{what}: fabric on minted no replacement"
        );
    }
    let _ = std::fs::remove_file(&key);
    write(&undamaged);
    assert_eq!(s.bodies(KEY).len(), 3, "the key back, it reads");
}

/// A key that is not the log's refuses at the open, with the file untouched, and the remedy says
/// the key may be wrong rather than send the person to a cut.
#[test]
fn a_wrong_key_refuses_and_its_remedy_is_the_keys() {
    let s = sealed();
    let before = s.snapshot();
    let refused = durability::open_escalating(
        &s.log(),
        None,
        Durability::Ordered,
        Some([7; 16]),
        Vec::new(),
        Some(OTHER),
    )
    .err()
    .expect("another key refuses");
    assert_eq!(refused.kind(), io::ErrorKind::InvalidData, "{refused}");
    assert_eq!(s.snapshot(), before);
    let remedy = open_remedy(&s.log(), refused.kind()).expect("a remedy");
    assert!(
        remedy.starts_with(SEALED_LOG_REMEDY)
            && remedy.contains("--retire-log")
            && remedy.contains(&*s.key().to_string_lossy()),
        "{remedy}"
    );
    assert!(
        remedy.contains("--repair") && remedy.contains("the records from it on are lost"),
        "a cut says what it costs: {remedy}"
    );
    let cut = astream_broker::BrokerLog::open_encrypted_repair(
        s.log(),
        astream_broker::Durability::Strict,
        OTHER,
    )
    .err()
    .expect("a repair under another key refuses");
    assert_eq!(cut.kind(), io::ErrorKind::InvalidData, "{cut}");
    assert_eq!(s.snapshot(), before, "and cuts nothing");
}

/// A sealed log is never opened keyless, with its `.seal` or without it, and a keyed open of one
/// whose `.seal` is gone refuses too: a deleted binding reads as nothing older.
#[test]
fn a_sealed_log_is_never_opened_keyless() {
    let s = sealed();
    let open = |key| {
        durability::open_escalating(
            &s.log(),
            None,
            Durability::Ordered,
            Some([7; 16]),
            Vec::new(),
            key,
        )
        .err()
    };
    let before = s.snapshot();
    assert_eq!(
        open(None).expect("keyless refuses").kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(s.snapshot(), before);
    std::fs::remove_file(seal_path(&s.log())).expect("remove the seal");
    let before = s.snapshot();
    assert_eq!(
        open(None).expect("keyless refuses with no seal").kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        open(Some(KEY)).expect("unbound refuses").kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(s.snapshot(), before);
    assert!(
        before.iter().any(|(name, _)| name == "bus.log.held"),
        "a real broker's `.held` is among what stays: {:?}",
        before.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
    // A start beside the key takes it for a plaintext log, and its seal refuses to read it: the
    // log stays as it is, the keyless open that follows refuses, and the note says that rather
    // than promise a plaintext log that will not be served.
    let prepared = prepare(&s.log(), Backups::INERT).expect("prepared");
    assert!(prepared.key.is_none(), "{:?}", prepared.notes);
    assert_eq!(s.snapshot(), before, "the failed seal changed nothing");
    let note = prepared.notes.join("\n");
    assert!(
        note.contains("not sealed at rest") && note.contains("refused by the open that follows"),
        "{note}"
    );
    assert!(!note.contains("untouched"), "{note}");
    assert_eq!(
        open(None).expect("and the open refuses").kind(),
        io::ErrorKind::InvalidData
    );
}

/// A sealed log whose `.seal` is gone: the plain remedy reads as a corrupt log, whose keyless repair
/// refuses and cuts nothing, and that refusal names the way out, which works.
#[test]
fn a_log_whose_seal_is_gone_is_told_how_to_give_it_up() {
    let s = sealed();
    std::fs::remove_file(seal_path(&s.log())).expect("remove the seal");
    let before = s.snapshot();
    assert_eq!(open_remedy(&s.log(), io::ErrorKind::InvalidData), None);
    let refused =
        astream_broker::BrokerLog::open_repair(s.log(), astream_broker::Durability::Strict)
            .err()
            .expect("the keyless repair refuses");
    assert_eq!(refused.kind(), io::ErrorKind::InvalidData, "{refused}");
    assert_eq!(s.snapshot(), before, "nothing cut");
    let remedy = repair_remedy(&s.log(), refused.kind()).expect("a remedy");
    assert!(
        remedy.contains(RETIRE_VERB) && remedy.contains(".seal"),
        "{remedy}"
    );
    let retired = retire(&s.log(), Backups::INERT).expect("it retires");
    assert_eq!(
        std::fs::read(retired.to.join("bus.log")).expect("kept aside"),
        before
            .iter()
            .find(|(name, _)| name == "bus.log")
            .expect("the log")
            .1,
        "its records are kept"
    );
}

/// A damaged plaintext log beside a key keeps the plain remedy, and its keyless repair runs.
#[test]
fn a_damaged_plaintext_log_beside_a_key_has_the_plain_repair() {
    let s = Scratch::new();
    s.fill(None, 2);
    let mut bytes = std::fs::read(s.log()).expect("the log");
    bytes[20] ^= 0xFF;
    std::fs::write(s.log(), &bytes).expect("damage it");
    s.plant_key(&KEY);
    let prepared = prepare(&s.log(), Backups::INERT).expect("prepared");
    assert!(prepared.key.is_none(), "{:?}", prepared.notes);
    assert!(!is_sealed(&s.log()));
    assert_eq!(open_remedy(&s.log(), io::ErrorKind::InvalidData), None);
    let (_, dropped) =
        astream_broker::BrokerLog::open_repair(s.log(), astream_broker::Durability::Strict)
            .expect("the plain repair runs");
    assert!(dropped > 0);
}

/// No key is minted beside a sealed log, and none over a key.
#[test]
fn a_mint_never_strands_a_sealed_log_or_replaces_a_key() {
    let s = sealed();
    std::fs::remove_file(s.key()).expect("lose the key");
    let refused = mint(&s.log(), Backups::INERT).expect_err("sealed");
    assert_eq!(refused.kind(), io::ErrorKind::InvalidInput, "{refused}");
    assert!(!s.key().exists());

    let plain = Scratch::new();
    plain.plant_key(&KEY);
    assert!(matches!(mint(&plain.log(), Backups::INERT), Ok(None)));
    assert_eq!(std::fs::read(plain.key()).expect("kept"), KEY);
}

/// Two mints race: one key stands, and neither overwrote it.
#[test]
fn a_mint_makes_one_whole_key_and_marks_it_before_its_name() {
    let s = Scratch::new();
    assert!(
        matches!(
            mint(&s.log(), Backups::SYSTEM),
            Ok(Some(marked)) if marked.is_ok() == Backups::system_has_a_mark()
        ),
        "the key is minted, and what the mark did is told"
    );
    let first = std::fs::read(s.key()).expect("minted");
    assert_eq!(first.len(), KEY_LEN);
    assert!(matches!(mint(&s.log(), Backups::SYSTEM), Ok(None)));
    assert_eq!(std::fs::read(s.key()).expect("kept"), first);
    let names: Vec<_> = s.snapshot().into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, [REST_KEY], "no temporary left");
    #[cfg(target_os = "macos")]
    {
        let file = std::fs::File::open(s.key()).expect("open");
        assert!(Backups::SYSTEM.excluded(&file), "left out of backups");
        let plain = Scratch::new();
        assert!(matches!(
            mint(&plain.log(), Backups::INERT),
            Ok(Some(Ok(())))
        ));
        let file = std::fs::File::open(plain.key()).expect("open");
        assert!(
            !Backups::SYSTEM.excluded(&file),
            "the inert arm marks nothing"
        );
    }
}

/// A log a broker holds is not sealed under it: this start's open says what it always said.
#[test]
fn a_held_log_is_not_sealed_under_its_broker() {
    let s = Scratch::new();
    s.fill(None, 1);
    let holder =
        durability::open(&s.log(), None, Durability::Ordered, Some([7; 16])).expect("open");
    s.plant_key(&KEY);
    let prepared = prepare(&s.log(), Backups::INERT).expect("prepared");
    assert!(prepared.key.is_none());
    assert!(prepared.notes.is_empty(), "{:?}", prepared.notes);
    assert!(!is_sealed(&s.log()));
    let refused = durability::open(&s.log(), None, Durability::Ordered, Some([7; 16]))
        .err()
        .expect("held");
    assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists);
    drop(holder);
}

/// A seal that cannot run leaves the log serving plaintext, every record there, and says so; the
/// next start tries again and takes it.
#[test]
fn a_seal_that_cannot_run_keeps_serving_plaintext_and_tries_again() {
    let s = Scratch::new();
    let identity = s.fill(None, 2);
    s.plant_key(&KEY);
    // astream refuses to seal beside an anti-rollback watermark.
    let mut watermark = s.log().into_os_string();
    watermark.push(".hw");
    std::fs::write(&watermark, b"").expect("watermark");
    let before = s.but_for_the_last_run();
    let prepared = prepare(&s.log(), Backups::INERT).expect("prepared");
    assert!(prepared.key.is_none());
    assert!(
        prepared
            .notes
            .iter()
            .any(|n| n.contains("serves plaintext")),
        "{:?}",
        prepared.notes
    );
    assert_eq!(s.but_for_the_last_run(), before, "the log untouched");
    // Sequence 3 is the one new record: the two before it are this producer's duplicates.
    assert_eq!(s.fill(None, 3), identity, "the same log serves on");
    std::fs::remove_file(&watermark).expect("remove");
    let prepared = prepare(&s.log(), Backups::INERT).expect("prepared");
    assert!(prepared.key.is_some(), "{:?}", prepared.notes);
    assert!(is_sealed(&s.log()));
    assert_eq!(
        s.bodies(KEY).len(),
        3,
        "every record, from before and after"
    );
}

/// `aterm fabric on`'s step: a dry run writes nothing, the real run mints once and keeps it.
#[test]
fn the_key_step_mints_once_and_a_dry_run_writes_nothing() {
    let s = Scratch::new();
    let dry = ensure_key(&s.log(), Backups::INERT, true).expect("dry");
    assert!(dry.minted);
    assert!(!s.key().exists());
    assert!(
        ensure_key(&s.log(), Backups::INERT, false)
            .expect("run")
            .minted
    );
    let key = std::fs::read(s.key()).expect("minted");
    assert!(
        !ensure_key(&s.log(), Backups::INERT, false)
            .expect("again")
            .minted
    );
    assert_eq!(std::fs::read(s.key()).expect("kept"), key);
}

/// Retiring a sealed log whose key is gone moves all of it aside, nothing deleted, and the next
/// start begins a sealed log with a new identity; the key found later reads the retired one.
#[test]
fn a_retired_log_is_kept_and_the_next_start_is_a_new_identity() {
    let s = sealed();
    let old_identity = {
        let log = astream_broker::BrokerLog::open_encrypted_bound(s.log(), KEY).expect("opens");
        log.identity().to_string()
    };
    std::fs::remove_file(s.key()).expect("lose the key");
    let retired = retire(&s.log(), Backups::INERT).expect("retired");
    assert!(retired.minted);
    assert!(retired.to.join("bus.log").is_file());
    assert!(retired.to.join("bus.log.seal").is_file());
    assert!(!s.log().exists() && !seal_path(&s.log()).exists());
    assert!(s.key().exists(), "a new key waits for the new log");

    let prepared = prepare(&s.log(), Backups::INERT).expect("prepared");
    let identity = s.fill(prepared.key.as_ref().map(Key::bytes), 1);
    assert_ne!(identity, old_identity, "a new identity");
    assert!(is_sealed(&s.log()));

    let old = astream_broker::BrokerLog::open_encrypted_bound(retired.to.join("bus.log"), KEY)
        .expect("the old key reads the retired log");
    assert_eq!(old.read_from(old.base()).len(), 3);
}

/// A damaged key goes aside with the log: the file the person may still repair is kept.
#[test]
fn a_retired_log_keeps_the_key_it_was_refused_with() {
    let s = sealed();
    let _ = std::fs::remove_file(s.key());
    s.plant_key(&OTHER);
    let retired = retire(&s.log(), Backups::INERT).expect("retired");
    assert_eq!(
        std::fs::read(retired.to.join(REST_KEY)).expect("kept"),
        OTHER
    );
    assert_ne!(std::fs::read(s.key()).expect("new"), OTHER);
}

/// A log that opens is not given up, and neither is one a broker holds; both stay as they were.
#[test]
fn a_log_that_opens_or_is_held_is_not_retired() {
    let s = sealed();
    let identity = {
        let log = astream_broker::BrokerLog::open_encrypted_bound(s.log(), KEY).expect("opens");
        log.identity().to_string()
    };
    let before = s.but_for_the_last_run();
    let why = retire(&s.log(), Backups::INERT).expect_err("it opens");
    assert!(why.contains("nothing to give up"), "{why}");
    assert_eq!(s.but_for_the_last_run(), before);
    let log = astream_broker::BrokerLog::open_encrypted_bound(s.log(), KEY).expect("opens");
    assert_eq!(log.identity().to_string(), identity, "the same log");
    drop(log);

    let plain = Scratch::new();
    plain.fill(None, 1);
    let why = retire(&plain.log(), Backups::INERT).expect_err("it opens");
    assert!(why.contains("nothing to give up"), "{why}");

    let holder =
        durability::open(&plain.log(), None, Durability::Ordered, Some([7; 16])).expect("open");
    let why = retire(&plain.log(), Backups::INERT).expect_err("held");
    assert!(why.contains("held by a running broker"), "{why}");
    drop(holder);

    let none = Scratch::new();
    let why = retire(&none.log(), Backups::INERT).expect_err("no log");
    assert!(why.contains("bus.log"), "{why}");
    assert!(!none.key().exists());
}

/// A log's rules, by aterm's shape and small enough for a tick to cut: the bulk stream by its
/// bytes, mail held for its reader, and the default rule. What `.held` and `.state` are for.
fn small_rules() -> astream_broker::Retention {
    use astream_broker::retention::{Retention, Rule};
    Retention::new(
        vec![
            Rule::prefix("/f/F/pub/*/*/ev")
                .expect("a rule")
                .max_bytes(2_000),
            Rule::hold("/f/F/in/>")
                .expect("a rule")
                .max_age(std::time::Duration::from_secs(7 * 24 * 60 * 60)),
        ],
        Rule::default_hold(),
    )
    .expect("rules")
}

/// Mail a tick carries into `.held`, a quiet subject it carries into `.state`, each with the
/// sentinel, and a stream that takes the cut past them both.
fn compact(mut log: astream_broker::BrokerLog) {
    log.set_retention(small_rules()).expect("the rules");
    log.publish(9, 1, "/a/q/quiet".into(), SENTINEL.to_vec())
        .expect("quiet");
    log.publish(9, 2, "/f/F/in/n1/from/a".into(), SENTINEL.to_vec())
        .expect("mail");
    for seq in 0..60 {
        log.publish(7, seq + 10, "/f/F/pub/n1/s1/ev".into(), vec![b'e'; 64])
            .expect("the stream");
    }
    let tick = log
        .retain_by_rules(1_800_000_000_000)
        .expect("the rules cut");
    assert!(tick.base > 2, "the tick cut past both: {tick:?}");
    drop(log);
}

/// NEGATIVE CONTROL for `.state` and `.held`: a plaintext log that compacted holds the sentinel in
/// each. The start that finds a key seals both, and a log sealed from its first record never held
/// it in any of them. (`fabric_world`'s privacy sweep compacts nothing, so it sees neither.)
#[test]
fn the_sidecars_a_compaction_leaves_hold_the_sentinel_plaintext_and_none_sealed() {
    let s = Scratch::new();
    compact(astream_broker::BrokerLog::open(s.log()).expect("plaintext"));
    assert!(s.holds_sentinel(".state"), "CONTROL: `.state` holds it");
    assert!(s.holds_sentinel(".held"), "CONTROL: `.held` holds it");

    s.plant_key(&KEY);
    assert!(prepare(&s.log(), Backups::INERT)
        .expect("prepared")
        .key
        .is_some());
    assert!(is_sealed(&s.log()));
    for suffix in ["", ".state", ".held"] {
        assert!(!s.holds_sentinel(suffix), "sealed: {suffix:?} holds it");
    }

    let fresh = Scratch::new();
    compact(astream_broker::BrokerLog::open_encrypted_bound(fresh.log(), KEY).expect("sealed"));
    for suffix in ["", ".state", ".held"] {
        assert!(!fresh.holds_sentinel(suffix), "fresh: {suffix:?} holds it");
    }
}

/// Every folder in `s` that a retire made.
fn retired_folders(s: &Scratch) -> Vec<PathBuf> {
    std::fs::read_dir(s.dir.path())
        .expect("list")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("retired-"))
        })
        .collect()
}

/// A folder or a file that never was a log is refused, moved nowhere: `--repair` refuses the same
/// folder. The folder that holds a running broker's log is the worst case, since the flock on the
/// folder's own descriptor would not see that broker's hold on the log inside.
#[test]
fn only_a_log_is_retired() {
    let s = sealed();
    let holder = durability::open_escalating(
        &s.log(),
        None,
        Durability::Ordered,
        Some([7; 16]),
        Vec::new(),
        Some(KEY),
    )
    .expect("a broker holds the log");
    let why = retire(s.dir.path(), Backups::INERT).expect_err("a folder");
    assert!(why.contains("is not a file"), "{why}");
    assert!(s.log().exists() && s.dir.path().is_dir());
    drop(holder);

    let notes = s.dir.path().join("notes.txt");
    let bak = s.dir.path().join("notes.txt.bak");
    std::fs::write(&notes, b"never a log").expect("notes");
    std::fs::write(&bak, b"its copy").expect("copy");
    let why = retire(&notes, Backups::INERT).expect_err("never a log");
    assert!(why.contains("no bus log"), "{why}");
    assert!(notes.exists() && bak.exists());

    let link = s.dir.path().join("link.log");
    std::os::unix::fs::symlink(s.log(), &link).expect("link");
    let why = retire(&link, Backups::INERT).expect_err("a link");
    assert!(why.contains("is not a file"), "{why}");

    assert_eq!(retired_folders(&s), Vec::<PathBuf>::new(), "no folder made");
    assert!(!beside(&s.log(), RETIRING).exists());
}

/// A running broker's sealed log whose key file is gone is the one case no open answers for: there
/// is no key to try it with, so the lock alone refuses. Moved aside, the log would go on taking
/// that broker's writes in the retired folder while the next start began a new one.
#[test]
fn a_held_sealed_log_whose_key_is_gone_is_not_retired() {
    let s = sealed();
    let holder = durability::open_escalating(
        &s.log(),
        None,
        Durability::Ordered,
        Some([7; 16]),
        Vec::new(),
        Some(KEY),
    )
    .expect("a broker holds the log");
    std::fs::remove_file(s.key()).expect("lose the key");
    let before = s.snapshot();
    let why = retire(&s.log(), Backups::INERT).expect_err("held");
    assert!(why.contains("held by a running broker"), "{why}");
    assert_eq!(s.snapshot(), before);
    assert_eq!(retired_folders(&s), Vec::<PathBuf>::new());
    assert!(!s.key().exists(), "and no key minted");
    drop(holder);
    assert!(retire(&s.log(), Backups::INERT).is_ok(), "once it is free");
}

/// A CRASH AT ANY POINT OF A RETIRE: after each move, a start and `fabric on` refuse, naming the
/// verb, and mint nothing beside sealed records that may have lost their `.seal`; a `--repair` is
/// refused the same way (`cli.rs`, on the same answer). Running it again moves the rest to the same
/// folder, so the key found later reads it there, and only then does a start begin a new log.
#[test]
fn a_retire_a_crash_stopped_refuses_every_start_and_finishes_in_its_one_folder() {
    for key_gone in [true, false] {
        let names = |s: &Scratch| retire_names(&s.log()).expect("names");
        let stage = || {
            let s = sealed();
            std::fs::remove_file(s.key()).expect("the key");
            if !key_gone {
                s.plant_key(&OTHER);
            }
            s
        };
        let total = names(&stage()).len();
        assert!(total >= 3, "the sidecars, the log and more");
        for crashed_after in 0..=total {
            let s = stage();
            let to = begin_retire(&s.log()).expect("begun");
            for entry in names(&s).iter().take(crashed_after) {
                std::fs::rename(s.dir.path().join(entry), to.join(entry)).expect("moved");
            }
            let at = format!("key_gone={key_gone} crashed_after={crashed_after}");
            let keyed = s
                .key()
                .exists()
                .then(|| std::fs::read(s.key()).expect("key"));

            let refused = prepare(&s.log(), Backups::INERT).expect_err(&at);
            assert!(
                refused.remedy.starts_with(UNFINISHED_RETIRE_REMEDY)
                    && refused.remedy.contains(RETIRE_VERB),
                "{at}: {refused:?}"
            );
            assert_eq!(
                ensure_key(&s.log(), Backups::INERT, false).expect_err(&at),
                refused
            );
            assert_eq!(
                ensure_key(&s.log(), Backups::INERT, true).expect_err(&at),
                refused
            );
            assert_eq!(
                s.key()
                    .exists()
                    .then(|| std::fs::read(s.key()).expect("key")),
                keyed,
                "{at}: no key minted or changed"
            );

            let retired = retire(&s.log(), Backups::INERT).expect(&at);
            assert_eq!(retired.to, to, "{at}: one folder");
            assert_eq!(retired_folders(&s), vec![to.clone()], "{at}");
            assert!(retired.minted, "{at}");
            assert!(!beside(&s.log(), RETIRING).exists(), "{at}");
            assert!(
                retired.moved.iter().any(|n| n == "bus.log")
                    && retired.moved.iter().any(|n| n == "bus.log.seal"),
                "{at}: {:?}",
                retired.moved
            );
            let old = astream_broker::BrokerLog::open_encrypted_bound(to.join("bus.log"), KEY)
                .expect("the key found later reads it");
            assert_eq!(old.read_from(old.base()).len(), 3, "{at}");
            drop(old);

            let prepared = prepare(&s.log(), Backups::INERT).expect(&at);
            assert!(prepared.key.is_some(), "{at}");
        }
    }
}

/// NEGATIVE CONTROL: a marker that names no canonical `retired-<digits>` folder, or is a
/// symlink, or resumes onto a non-regular `log`/sidecar, moves nothing.
#[test]
fn a_retires_marker_is_read_strictly() {
    let s = sealed();
    std::fs::remove_file(s.key()).expect("the key");
    let marker = beside(&s.log(), RETIRING);
    for bad in [
        "../elsewhere",
        "notes",
        "retired-1/../..",
        "retired-",
        "retired-1\n",
        "retired-1\\..\\..",
        "retired-abc",
    ] {
        std::fs::write(&marker, bad).expect("marker");
        let why = retire(&s.log(), Backups::INERT).expect_err(bad);
        assert!(why.contains("names no retired folder"), "{why}");
        assert!(s.log().exists(), "{bad}: nothing moved");
    }
    std::fs::remove_file(&marker).expect("marker");

    let target_marker = s.dir.path().join("marker-target");
    std::fs::write(&target_marker, "retired-123").expect("target marker");
    std::os::unix::fs::symlink(&target_marker, &marker).expect("symlink marker");
    let why = retire(&s.log(), Backups::INERT).expect_err("symlink marker");
    assert!(why.contains("not a regular file"), "{why}");
    assert!(s.log().exists(), "symlink marker moves nothing");
    std::fs::remove_file(&marker).expect("remove symlink marker");

    let to = begin_retire(&s.log()).expect("begin");
    let saved_log = s.dir.path().join("saved-bus-log");
    std::fs::rename(s.log(), &saved_log).expect("set aside log");
    std::os::unix::fs::symlink(&saved_log, s.log()).expect("symlink at log");
    let why = retire(&s.log(), Backups::INERT).expect_err("symlink at log on resume");
    assert!(why.contains("is not a file"), "{why}");
    std::fs::remove_file(s.log()).expect("remove symlink at log");
    std::fs::rename(&saved_log, s.log()).expect("restore log");
    let retired = retire(&s.log(), Backups::INERT).expect("clean resume");
    assert_eq!(retired.to, to);
}

/// A bare relative name's folder is the working one, not the empty path every folder call refuses.
#[test]
fn a_bare_log_name_sits_in_the_working_folder() {
    assert_eq!(folder_of(Path::new("bus.log")), Path::new("."));
    assert_eq!(folder_of(Path::new("a/bus.log")), Path::new("a"));
    assert_eq!(folder_of(Path::new("/r/bus.log")), Path::new("/r"));
}

/// A KEY A CRASH INSIDE ITS MINT LEFT WITH A SECOND LINK starts, and loses the temporary: the
/// reader refuses a second link, and launchd's restarts would have met that refusal for good with
/// no log sealed under it. A live minter's temporary is left alone, and that read refuses for its
/// moment.
#[test]
fn a_key_a_crash_left_with_a_second_link_reads_and_loses_its_temporary() {
    use std::os::unix::fs::MetadataExt as _;
    let s = Scratch::new();
    assert!(matches!(mint(&s.log(), Backups::INERT), Ok(Some(Ok(())))));
    let key = std::fs::read(s.key()).expect("minted");
    let dead = s
        .dir
        .path()
        .join(format!(".{REST_KEY}.tmp.{}.0a1b2c3d", u32::MAX));
    let link_the_temporary = |temp: &Path| std::fs::hard_link(s.key(), temp).expect("linked");

    link_the_temporary(&dead);
    assert!(
        matches!(
            read_with(&s.log(), crate::transport::read_mint_secret),
            KeyFile::Damaged(_)
        ),
        "CONTROL: the reader refuses a second link"
    );
    let prepared = prepare(&s.log(), Backups::INERT).expect("it starts");
    assert_eq!(
        prepared.key.as_ref().expect("the key").bytes().to_vec(),
        key
    );
    assert!(!dead.exists(), "the temporary is gone");
    assert_eq!(std::fs::metadata(s.key()).expect("key").nlink(), 1);

    link_the_temporary(&dead);
    assert!(
        ensure_key(&s.log(), Backups::INERT, false).is_ok(),
        "`fabric on` takes the same step"
    );
    assert!(!dead.exists());

    let live = s
        .dir
        .path()
        .join(format!(".{REST_KEY}.tmp.{}.0a1b2c3d", std::process::id()));
    link_the_temporary(&live);
    let refused = prepare(&s.log(), Backups::INERT).expect_err("a live writer's moment");
    assert!(refused.why.contains("does not read"), "{refused:?}");
    assert!(live.exists(), "a live writer's temporary stays");
}

/// Where the system has no mark a key is still minted, and every step says what that means rather
/// than report a key left out of backups that a backup copies.
#[test]
fn a_system_with_no_backup_mark_says_so_at_every_step() {
    let s = Scratch::new();
    aterm_uds::private_file::write_private(s.dir.path(), crate::enable::MINT_SECRET, &[1; KEY_LEN])
        .expect("secret");
    let unmarked = |notes: &[String]| notes.iter().any(|n| n.contains("beside the log it seals"));
    let first = prepare(&s.log(), Backups::NO_MARK).expect("prepared");
    assert!(first.key.is_some() && s.key().exists());
    assert!(unmarked(&first.notes), "{:?}", first.notes);
    let again = prepare(&s.log(), Backups::NO_MARK).expect("prepared");
    assert!(unmarked(&again.notes), "{:?}", again.notes);

    let step = ensure_key(&s.log(), Backups::NO_MARK, false).expect("kept");
    assert!(step
        .unmarked
        .is_some_and(|why| why.contains("beside the log it seals")));
    let fresh = Scratch::new();
    let step = ensure_key(&fresh.log(), Backups::NO_MARK, false).expect("minted");
    assert!(step.minted && step.unmarked.is_some());

    assert!(
        prepare(&s.log(), Backups::INERT)
            .expect("prepared")
            .notes
            .is_empty(),
        "CONTROL: the inert arm has nothing to say"
    );
}
