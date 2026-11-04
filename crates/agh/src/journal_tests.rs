// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The write journal's in-crate tests (plan §8): its frames, its lock, torn tails and faults,
//! durability, the re-judging `begin`, the bounds, the settle window and the listing. Each
//! draft is built here as a literal, which needs no `Permit`; `tests/journal.rs` builds its
//! drafts through `IntentDraft::new` over a permit once `Router::permit` is written (plan §2).

use std::sync::Arc;
use std::time::Duration;

use aterm_durable::Flush;
use aterm_durable::seam::Recording;

use super::*;
use crate::error::Kind;
use crate::needs;

/// The simulated clock's start, as `tests/common.rs` names it.
const SIM_START: u64 = 1_760_000_000;
/// The hang detector for a wait that must end (plan §1).
const HANG: Duration = Duration::from_secs(60);

/// A scratch root.
fn scratch() -> aterm_tempfile::TempDir {
    crate::test_fixtures::scratch_dir("journal")
}

/// `fixture-dev/app`.
fn app() -> Slug {
    Slug::parse("fixture-dev/app").expect("a slug")
}

/// A simulated clock at `SIM_START`.
fn fx() -> Arc<Fx> {
    Arc::new(Fx::simulated(SIM_START))
}

/// The request digest every test draft names: `sha256:` and 64 zeros, then ` 812`.
fn request() -> String {
    format!("sha256:{} 812", "0".repeat(64))
}

/// A draft of `op` and `key` on `fixture-dev/app`, as `IntentDraft::new` builds one under a
/// permit for the profile `dev`, with the request filled.
fn draft(op: WriteOp, key: &str) -> IntentDraft {
    IntentDraft {
        op,
        slug: app(),
        identity: "dev".to_string(),
        fingerprint: "fixture-fingerprint".to_string(),
        key: key.to_string(),
        request: request(),
        target_commitish: None,
        body_digest: None,
        grant: None,
        after: None,
    }
}

/// `begin` as a caller does: `after` is the id of `latest(op, key)` at the time.
fn begin(journal: &Journal, draft: IntentDraft) -> IntentId {
    let after = journal
        .latest(draft.op, &draft.key)
        .expect("latest")
        .map(|view| view.intent.id);
    journal
        .begin(&draft.with_after(after))
        .unwrap_or_else(|e| panic!("begin: {}", e.why))
}

/// The journal of `fixture-dev/app` under `root`.
fn journal(root: &Path, fx: &Arc<Fx>) -> Journal {
    Journal::open(&Dirs::under(root), &app(), fx).unwrap_or_else(|e| panic!("open: {}", e.why))
}

/// The first 12 hex of sha256(`slug` 0x00 `op` 0x00 `key`).
fn hex12(slug: &str, op: &str, key: &str) -> String {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(slug.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(op.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(key.as_bytes());
    let hex = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&bytes));
    hex[..12].to_string()
}

/// The file's bytes.
fn bytes(journal: &Journal) -> Vec<u8> {
    std::fs::read(journal.path()).expect("the journal's bytes")
}

/// Make the file hold exactly `all`, created 0600 as the journal makes it when missing.
fn write_raw(journal: &Journal, all: &[u8]) {
    use std::os::unix::fs::FileExt as _;
    let path = journal.path();
    let dir = path.parent().expect("the owner dir");
    let name = path.file_name().and_then(|n| n.to_str()).expect("a name");
    let file = aterm_uds::private_file::open_private_rw(dir, name).expect("the journal");
    file.set_len(0).expect("emptied");
    file.write_all_at(all, 0).expect("written");
}

/// Append `extra` to the file, as a crash or a person would leave it.
fn append_raw(journal: &Journal, extra: &[u8]) {
    use std::os::unix::fs::FileExt as _;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(journal.path())
        .expect("the journal");
    let len = file.metadata().expect("metadata").len();
    file.write_all_at(extra, len).expect("appended");
}

/// Fill the file, after its records, with valid frames written straight to it (no fsync per
/// record) until it ends less than one small frame below `target`: Settled Unknown records
/// of an id no Intent holds, long ones first, then Retracted ones. Answers its length.
fn fill_to(journal: &Journal, target: u64) -> u64 {
    let stranger = IntentId::parse("0-000000000000").expect("an id");
    let long = encode(&JournalRecord::Settled {
        intent: stranger.clone(),
        outcome: Settled::Unknown {
            why: "x".repeat(3000),
        },
    });
    let short = encode(&JournalRecord::Retracted {
        intent: stranger,
        at_secs: 0,
    });
    let (records, end) = journal.read().expect("read");
    assert_eq!(end, JournalEnd::Clean);
    let mut step = u64::try_from(records.len()).expect("fits");
    let mut len = u64::try_from(bytes(journal).len()).expect("fits");
    let mut tail = Vec::new();
    for body in [&long, &short] {
        let frame_len = u64::try_from(FRAME_OVERHEAD + body.len()).expect("fits");
        while len + frame_len <= target {
            tail.extend_from_slice(&astream_wire::steps::encode_step(step, body).expect("a frame"));
            step += 1;
            len += frame_len;
        }
    }
    append_raw(journal, &tail);
    assert_eq!(
        std::fs::metadata(journal.path()).expect("metadata").len(),
        len
    );
    len
}

/// The keys `needs::list` answers.
fn need_keys(dirs: &Dirs) -> Vec<String> {
    needs::list(dirs)
        .expect("needs")
        .into_iter()
        .map(|n| n.key)
        .collect()
}

/// The intents a read answers.
fn intents(journal: &Journal) -> Vec<Intent> {
    let (records, _end) = journal.read().expect("read");
    records
        .into_iter()
        .filter_map(|(_, record)| match record {
            JournalRecord::Intent(intent) => Some(*intent),
            _ => None,
        })
        .collect()
}

#[test]
fn record_bodies_are_exact() {
    let id = IntentId::parse("0-1a2b3c4d5e6f").expect("an id");
    let notes = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(b"notes"));
    let zeros = "0".repeat(64);
    let intent = JournalRecord::Intent(Box::new(Intent {
        id: id.clone(),
        op: WriteOp::ReleaseCreate,
        slug: Slug::parse("alabsystems/aterm").expect("a slug"),
        identity: "release".to_string(),
        fingerprint: "fixture-fingerprint".to_string(),
        key: "v1".to_string(),
        request: request(),
        target_commitish: None,
        body_digest: Some(format!("sha256:{notes}")),
        grant: Some("fixture-caller".to_string()),
        at_secs: SIM_START,
    }));
    let url = "https://github.com/alabsystems/aterm/releases/tag/v1";
    let rows: Vec<(JournalRecord, String)> = vec![
        (
            intent,
            format!(
                r#"{{"at_secs":1760000000,"body_digest":"sha256:{notes}","fingerprint":"fixture-fingerprint","grant":"fixture-caller","id":"0-1a2b3c4d5e6f","identity":"release","key":"v1","op":"release.create","request":"sha256:{zeros} 812","slug":"alabsystems/aterm","target_commitish":null,"type":"intent","v":1}}"#
            ),
        ),
        (
            JournalRecord::Settled {
                intent: id.clone(),
                outcome: Settled::Created {
                    id: 5000,
                    url: url.to_string(),
                },
            },
            format!(
                r#"{{"id":5000,"intent":"0-1a2b3c4d5e6f","outcome":"created","type":"settled","url":"{url}","v":1}}"#
            ),
        ),
        (
            JournalRecord::Settled {
                intent: id.clone(),
                outcome: Settled::Refused { status: 422 },
            },
            r#"{"intent":"0-1a2b3c4d5e6f","outcome":"refused","status":422,"type":"settled","v":1}"#
                .to_string(),
        ),
        (
            JournalRecord::Settled {
                intent: id.clone(),
                outcome: Settled::NeverSent,
            },
            r#"{"intent":"0-1a2b3c4d5e6f","outcome":"never_sent","type":"settled","v":1}"#
                .to_string(),
        ),
        (
            JournalRecord::Settled {
                intent: id.clone(),
                outcome: Settled::Unknown {
                    why: "GitHub answered 502".to_string(),
                },
            },
            r#"{"intent":"0-1a2b3c4d5e6f","outcome":"unknown","type":"settled","v":1,"why":"GitHub answered 502"}"#
                .to_string(),
        ),
        (
            JournalRecord::Observed {
                intent: id.clone(),
                found: Some(5000),
                at_secs: 1_760_000_060,
            },
            r#"{"at_secs":1760000060,"found":5000,"intent":"0-1a2b3c4d5e6f","type":"observed","v":1}"#
                .to_string(),
        ),
        (
            JournalRecord::Retracted {
                intent: id,
                at_secs: 1_760_000_090,
            },
            r#"{"at_secs":1760000090,"intent":"0-1a2b3c4d5e6f","type":"retracted","v":1}"#
                .to_string(),
        ),
    ];
    for (record, line) in rows {
        let body = encode(&record);
        assert_eq!(String::from_utf8_lossy(&body), line);
        assert_eq!(decode(0, 0, &body).expect("decodes"), record);
    }
}

#[test]
fn the_journal_frames_are_astream_steps_byte_for_byte() {
    assert_eq!(astream_wire::hash::crc32_ieee(b"123456789"), 0xCBF4_3926);
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    begin(&journal, draft(WriteOp::ReleaseCreate, "v1"));
    let (records, end) = journal.read().expect("read");
    assert_eq!(end, JournalEnd::Clean);
    let [(0, record)] = records.as_slice() else {
        panic!("one record at step 0: {records:?}");
    };
    let body = encode(record);
    let mut payload = vec![0x01];
    payload.extend_from_slice(&0u64.to_le_bytes());
    payload.extend_from_slice(&u32::try_from(body.len()).expect("small").to_le_bytes());
    payload.extend_from_slice(&body);
    let mut frame = vec![0xA5, 0x71, 0x01, 0x00];
    frame.extend_from_slice(&u32::try_from(payload.len()).expect("small").to_le_bytes());
    frame.extend_from_slice(&astream_wire::hash::crc32_ieee(&payload).to_le_bytes());
    frame.extend_from_slice(&payload);
    assert_eq!(bytes(&journal), frame);
}

#[test]
fn steps_are_dense_and_ids_name_their_step() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    let hex = hex12("fixture-dev/app", "ref.create", "refs/tags/v1");
    let mut after = None;
    for step in 0..3u64 {
        let id = journal
            .begin(&draft(WriteOp::RefCreate, "refs/tags/v1").with_after(after.clone()))
            .unwrap_or_else(|e| panic!("begin: {}", e.why));
        assert_eq!(id.step(), step);
        assert_eq!(id.as_str(), format!("{step}-{hex}"));
        after = Some(id);
    }
    let (records, end) = journal.read().expect("read");
    assert_eq!(end, JournalEnd::Clean);
    let steps: Vec<u64> = records.iter().map(|(step, _)| *step).collect();
    assert_eq!(steps, [0, 1, 2]);
    assert!(
        records
            .iter()
            .all(|(_, r)| matches!(r, JournalRecord::Intent(_)))
    );

    let upper = scratch();
    let shouting = Slug::parse("FIXTURE-DEV/App").expect("a slug");
    let journal = Journal::open(&Dirs::under(upper.path()), &shouting, &fx).expect("open");
    let mut loud = draft(WriteOp::RefCreate, "refs/tags/v1");
    loud.slug = shouting;
    let id = journal.begin(&loud).expect("begin");
    assert_eq!(id.as_str(), format!("0-{hex}"));
    assert!(journal.path().ends_with("fixture-dev/app.steps"));
}

#[test]
fn a_torn_tail_is_cut_back() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    begin(&journal, draft(WriteOp::ReleaseCreate, "v1"));
    begin(&journal, draft(WriteOp::ReleaseCreate, "v2"));
    let at = u64::try_from(bytes(&journal).len()).expect("small");
    let torn = astream_wire::steps::encode_step(2, b"{\"v\":1}").expect("a frame");
    append_raw(&journal, &torn[..9]);
    let (records, end) = journal.read().expect("read");
    assert_eq!(records.len(), 2);
    assert_eq!(end, JournalEnd::TornRepaired { at });
    assert_eq!(
        std::fs::metadata(journal.path()).expect("metadata").len(),
        at
    );
    let id = begin(&journal, draft(WriteOp::ReleaseCreate, "v3"));
    assert_eq!(id.step(), 2);
}

#[test]
fn a_fault_is_refused_and_never_cut() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    begin(&journal, draft(WriteOp::ReleaseCreate, "v1"));
    begin(&journal, draft(WriteOp::ReleaseCreate, "v2"));
    let mut damaged = bytes(&journal);
    // A byte inside the first record's body: past the frame and step headers.
    damaged[FRAME_OVERHEAD + 3] ^= 0x01;
    write_raw(&journal, &damaged);

    let e = journal
        .append(&JournalRecord::Retracted {
            intent: IntentId::parse("0-000000000000").expect("an id"),
            at_secs: SIM_START,
        })
        .expect_err("a fault is refused");
    assert_eq!(e.exit(), 1);
    assert_eq!(e.sub, Sub::JournalFault { at: 0 });
    assert_eq!(bytes(&journal), damaged);
    let (records, end) = journal.read().expect("read");
    assert!(records.is_empty());
    assert!(matches!(end, JournalEnd::Fault { at: 0, .. }), "{end:?}");
    let e = journal.open().expect_err("open refuses a fault");
    assert_eq!(e.sub.slug().as_deref(), Some("journal_fault"));
    let e = journal
        .latest(WriteOp::ReleaseCreate, "v2")
        .expect_err("latest refuses a fault");
    assert_eq!(e.sub.slug().as_deref(), Some("journal_fault"));
    assert_eq!(bytes(&journal), damaged);
}

#[test]
fn view_refuses_a_journal_damaged_past_its_intent() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    let first = begin(&journal, draft(WriteOp::ReleaseCreate, "v1"));
    let first_len = bytes(&journal).len();
    let second = begin(&journal, draft(WriteOp::ReleaseCreate, "v2"));
    let third = begin(&journal, draft(WriteOp::ReleaseCreate, "v3"));
    assert!(journal.view(&first).expect("view").is_some_and(|v| v.open));
    let mut damaged = bytes(&journal);
    // A byte inside the middle record's body: the first still reads, and a whole frame
    // follows, so this is a fault and not a torn tail.
    damaged[first_len + FRAME_OVERHEAD + 3] ^= 0x01;
    write_raw(&journal, &damaged);

    let (records, end) = journal.read().expect("read");
    assert_eq!(records.len(), 1);
    assert!(matches!(end, JournalEnd::Fault { .. }), "{end:?}");
    for id in [&first, &second, &third] {
        let e = journal.view(id).expect_err("view refuses a fault");
        assert_eq!(e.exit(), 1, "{id}");
        assert_eq!(e.sub.slug().as_deref(), Some("journal_fault"), "{id}");
    }
    assert_eq!(bytes(&journal), damaged);
}

#[test]
fn a_body_that_is_no_record_is_a_fault() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    begin(&journal, draft(WriteOp::ReleaseCreate, "v1"));
    let frame = astream_wire::steps::encode_step(0, br#"{"v":2}"#).expect("a frame");
    write_raw(&journal, &frame);
    let (records, end) = journal.read().expect("read");
    assert!(records.is_empty());
    let JournalEnd::Fault { at: 0, why } = end else {
        panic!("a fault at 0: {end:?}");
    };
    assert!(why.starts_with("step 0:"), "{why}");

    // A valid intent body whose id names step 5, written at step 0.
    let other = scratch();
    let source = Journal::open(&Dirs::under(other.path()), &app(), &fx).expect("open");
    for _ in 0..6 {
        let key = format!("k{}", intents(&source).len());
        begin(&source, draft(WriteOp::ReleaseCreate, &key));
    }
    let (records, _) = source.read().expect("read");
    let body = encode(&records[5].1);
    let frame = astream_wire::steps::encode_step(0, &body).expect("a frame");
    write_raw(&journal, &frame);
    let (records, end) = journal.read().expect("read");
    assert!(records.is_empty());
    let JournalEnd::Fault { at: 0, why } = end else {
        panic!("a fault at 0: {end:?}");
    };
    assert!(why.starts_with("step 0:"), "{why}");
}

#[test]
fn a_held_lock_is_busy_after_the_wait() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx).with_lock_wait_for_tests(Duration::ZERO);
    begin(&journal, draft(WriteOp::ReleaseCreate, "v1"));
    let before = bytes(&journal);
    let owner_dir = journal
        .path()
        .parent()
        .expect("the owner dir")
        .to_path_buf();
    let record = JournalRecord::Retracted {
        intent: IntentId::parse("0-000000000000").expect("an id"),
        at_secs: SIM_START,
    };
    {
        let _held = aterm_uds::private_file::PrivateLock::take(
            &owner_dir,
            "app.steps.lock",
            Duration::ZERO,
            "busy",
        )
        .expect("the test's lock");
        let e = journal.append(&record).expect_err("busy");
        assert_eq!(e.exit(), 3);
        assert_eq!(e.sub, Sub::Local);
        assert!(e.why.contains(&journal.path().display().to_string()));
        assert_eq!(bytes(&journal), before);
    }
    assert_eq!(journal.append(&record).expect("the lock is free"), 1);
}

#[test]
fn two_appenders_take_turns() {
    let root = scratch();
    let fx = fx();
    let (done, finished) = std::sync::mpsc::channel();
    let threads: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|side| {
            let root = root.path().to_path_buf();
            let fx = fx.clone();
            let done = done.clone();
            std::thread::spawn(move || {
                // The lock wait is the hang detector's, never a latency budget (plan §1).
                let journal = journal(&root, &fx).with_lock_wait_for_tests(HANG);
                for i in 0..50 {
                    begin(
                        &journal,
                        draft(WriteOp::ReleaseCreate, &format!("{side}{i}")),
                    );
                }
                let _ = done.send(side);
            })
        })
        .collect();
    drop(done);
    for _ in 0..2 {
        finished.recv_timeout(HANG).expect("an appender finished");
    }
    for thread in threads {
        thread.join().expect("an appender");
    }
    let journal = journal(root.path(), &fx);
    let (records, end) = journal.read().expect("read");
    assert_eq!(end, JournalEnd::Clean);
    let steps: Vec<u64> = records.iter().map(|(step, _)| *step).collect();
    assert_eq!(steps, (0..100).collect::<Vec<u64>>());
    let ids: std::collections::BTreeSet<IntentId> =
        intents(&journal).into_iter().map(|i| i.id).collect();
    assert_eq!(ids.len(), 100);
}

#[test]
fn the_append_is_durable_before_it_answers() {
    let root = scratch();
    let fx = fx();
    let flushes = Recording::start();
    let first = journal(root.path(), &fx);
    assert_eq!(flushes.take(), [Flush::Durable; 3]);
    let _again = journal(root.path(), &fx);
    assert_eq!(flushes.take(), Vec::<Flush>::new());

    begin(&first, draft(WriteOp::ReleaseCreate, "v1"));
    assert_eq!(flushes.take(), [Flush::Durable, Flush::Durable]);
    begin(&first, draft(WriteOp::ReleaseCreate, "v2"));
    assert_eq!(flushes.take(), [Flush::Durable]);

    let other = journal(root.path(), &fx);
    begin(&other, draft(WriteOp::ReleaseCreate, "v3"));
    assert_eq!(flushes.take(), [Flush::Durable, Flush::Durable]);

    let torn = astream_wire::steps::encode_step(3, b"{\"v\":1}").expect("a frame");
    append_raw(&first, &torn[..9]);
    begin(&first, draft(WriteOp::ReleaseCreate, "v4"));
    assert_eq!(flushes.take(), [Flush::Durable, Flush::Durable]);
}

#[test]
fn reading_a_missing_journal_makes_nothing() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    let (records, end) = journal.read().expect("read");
    assert!(records.is_empty());
    assert_eq!(end, JournalEnd::Clean);
    let owner_dir = journal.path().parent().expect("the owner dir");
    let names: Vec<_> = std::fs::read_dir(owner_dir)
        .expect("the owner dir")
        .map(|e| e.expect("an entry").file_name())
        .collect();
    assert!(names.is_empty(), "{names:?}");
}

#[test]
fn begin_judges_again_under_the_lock() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    let first = journal
        .begin(&draft(WriteOp::ReleaseCreate, "v1"))
        .expect("the first begins");
    let e = journal
        .begin(&draft(WriteOp::ReleaseCreate, "v1"))
        .expect_err("the racing second is refused");
    assert_eq!(e.exit(), 13);
    assert_eq!(
        e.sub,
        Sub::IntentOpen {
            intent: first.to_string()
        }
    );
    assert_eq!(intents(&journal).len(), 1);

    journal
        .settle(&first, &Settled::Refused { status: 422 })
        .expect("settled");
    let e = journal
        .begin(&draft(WriteOp::ReleaseCreate, "v1"))
        .expect_err("the journal moved past after: None");
    assert_eq!(e.exit(), 13);
    assert_eq!(
        e.sub,
        Sub::IntentOpen {
            intent: first.to_string()
        }
    );
    journal
        .begin(&draft(WriteOp::ReleaseCreate, "v1").with_after(Some(first)))
        .expect("after the first, one begins");

    let reference = journal
        .begin(&draft(WriteOp::RefCreate, "refs/tags/v1"))
        .expect("a ref create");
    journal
        .begin(&draft(WriteOp::RefCreate, "refs/tags/v1").with_after(Some(reference)))
        .expect("a ref create begins beside its open one");
}

#[test]
fn begin_refuses_an_after_the_journal_does_not_hold() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    let judged = IntentId::parse("3-000000000000").expect("an id");
    let e = journal
        .begin(&draft(WriteOp::ReleaseCreate, "v1").with_after(Some(judged)))
        .expect_err("no intent of the key to be after");
    assert_eq!(e.exit(), 9);
    assert_eq!(e.kind, Kind::Conflict);
    let (records, end) = journal.read().expect("read");
    assert!(records.is_empty());
    assert_eq!(end, JournalEnd::Clean);
    assert!(
        !journal.path().exists(),
        "a refused begin leaves no journal"
    );
}

#[test]
fn begin_refuses_another_repositorys_draft() {
    let root = scratch();
    let fx = fx();
    let dirs = Dirs::under(root.path());
    let journal = journal(root.path(), &fx);
    let mut elsewhere = draft(WriteOp::ReleaseCreate, "v1");
    elsewhere.slug = Slug::parse("alabsystems/aterm").expect("a slug");
    let e = journal.begin(&elsewhere).expect_err("not this journal's");
    assert_eq!(e.exit(), 2);
    assert!(journals(&dirs).expect("journals").is_empty());
    let other = Journal::open(&dirs, &elsewhere.slug, &fx).expect("open");
    assert!(intents(&other).is_empty());
}

#[test]
fn an_intent_is_settled_once() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    let id = begin(&journal, draft(WriteOp::ReleaseCreate, "v1"));
    let unknown = Settled::Unknown {
        why: "no answer".to_string(),
    };
    journal.settle(&id, &unknown).expect("the first settle");
    let before = bytes(&journal);
    for again in [
        Settled::NeverSent,
        Settled::Refused { status: 422 },
        Settled::Created {
            id: 5000,
            url: "https://github.com/fixture-dev/app/releases/tag/v1".to_string(),
        },
        unknown.clone(),
    ] {
        let e = journal.settle(&id, &again).expect_err("settled already");
        assert_eq!(e.exit(), 2, "{again:?}");
        assert_eq!(bytes(&journal), before);
    }
    let open: Vec<IntentId> = journal
        .open()
        .expect("open")
        .into_iter()
        .map(|i| i.id)
        .collect();
    assert_eq!(open, std::slice::from_ref(&id));
    let e = journal
        .begin(&draft(WriteOp::ReleaseCreate, "v1").with_after(Some(id.clone())))
        .expect_err("an Unknown stays open");
    assert_eq!(e.exit(), 13);

    // A second Settled spliced in by hand closes nothing: a view reads the first.
    let spliced = JournalRecord::Settled {
        intent: id.clone(),
        outcome: Settled::NeverSent,
    };
    journal.append(&spliced).expect("appended raw");
    let view = journal.view(&id).expect("view").expect("held");
    assert_eq!(view.settled, Some(unknown));
    assert!(view.open);
}

#[test]
fn a_long_unknown_why_is_cut_to_fit() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    let id = begin(&journal, draft(WriteOp::ReleaseCreate, "v1"));
    // Control bytes, which JSON writes six to one, and a multibyte character at the cut.
    let mut why: String = std::iter::repeat_n('\u{1}', WHY_MAX - 1).collect();
    why.push('é');
    why.push_str(&"x".repeat(8000));
    journal
        .settle(&id, &Settled::Unknown { why: why.clone() })
        .expect("the outcome of a sent write always fits");
    let view = journal.view(&id).expect("view").expect("held");
    let Some(Settled::Unknown { why: kept }) = view.settled else {
        panic!("settled unknown: {:?}", view.settled);
    };
    assert!(kept.len() <= WHY_MAX + '…'.len_utf8(), "{}", kept.len());
    assert!(kept.ends_with('…'));
    assert!(why.starts_with(kept.trim_end_matches('…')));
    assert!(view.open);
}

#[test]
fn a_failed_flush_cuts_the_journal_back() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    {
        let _failing = aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Other);
        let e = journal
            .begin(&draft(WriteOp::ReleaseCreate, "v1"))
            .expect_err("the directory flush fails");
        assert_eq!(e.exit(), 3);
        assert_eq!(e.sub, Sub::Local);
        let path = journal.path().display().to_string();
        assert!(e.why.contains(&path), "{}", e.why);
        assert!(!journal.path().exists(), "the file the append made is gone");
    }
    assert!(intents(&journal).is_empty());
    let first = journal
        .begin(&draft(WriteOp::ReleaseCreate, "v1"))
        .expect("once the flush works");
    assert_eq!(first.step(), 0);

    // An existing file, through a value that has not synced its directory yet.
    let before = bytes(&journal);
    let other = Journal::open(&Dirs::under(root.path()), &app(), &fx).expect("open");
    {
        let _failing = aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Other);
        let e = other
            .begin(&draft(WriteOp::ReleaseCreate, "v2"))
            .expect_err("the directory flush fails");
        assert_eq!(e.exit(), 3);
        assert_eq!(e.sub, Sub::Local);
        let path = other.path().display().to_string();
        assert!(e.why.contains(&path), "{}", e.why);
    }
    assert_eq!(bytes(&journal), before, "cut back to the length it had");
    assert_eq!(begin(&other, draft(WriteOp::ReleaseCreate, "v2")).step(), 1);
}

#[test]
fn a_refused_call_leaves_no_journal() {
    let root = scratch();
    let fx = fx();
    let dirs = Dirs::under(root.path());
    let journal = journal(root.path(), &fx);
    let stranger = IntentId::parse("0-000000000000").expect("an id");
    let e = journal.retract(&stranger).expect_err("not held");
    assert_eq!(e.exit(), 6);
    let e = journal
        .settle(&stranger, &Settled::NeverSent)
        .expect_err("not held");
    assert_eq!(e.exit(), 2);
    let e = journal.observe(&stranger, None).expect_err("not held");
    assert_eq!(e.exit(), 2);
    assert!(!journal.path().exists());
    assert!(journals(&dirs).expect("journals").is_empty());
}

#[test]
fn the_journal_refuses_past_64_mib() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    begin(&journal, draft(WriteOp::ReleaseCreate, "v1"));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(journal.path())
        .expect("the journal");
    file.set_len(JOURNAL_MAX + 1).expect("sparse");
    let e = journal
        .append(&JournalRecord::Retracted {
            intent: IntentId::parse("0-000000000000").expect("an id"),
            at_secs: SIM_START,
        })
        .expect_err("full");
    assert_eq!(e.exit(), 1);
    assert_eq!(
        e.sub,
        Sub::JournalFull {
            len: JOURNAL_MAX + 1
        }
    );
    assert_eq!(
        std::fs::metadata(journal.path()).expect("metadata").len(),
        JOURNAL_MAX + 1
    );
}

#[test]
fn a_full_journal_still_closes_its_open_intents() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    let a = begin(&journal, draft(WriteOp::ReleaseCreate, "v1"));
    let len = fill_to(&journal, JOURNAL_MAX - JOURNAL_RESERVE);
    let gap = (JOURNAL_MAX - JOURNAL_RESERVE) - len;
    assert!(gap < 100, "{gap}");

    let e = journal
        .begin(&draft(WriteOp::ReleaseCreate, "v2"))
        .expect_err("no Intent fits");
    assert_eq!(e.sub, Sub::JournalFull { len });
    assert!(e.next.contains("agh intent list") && e.next.contains("--open"));
    assert_eq!(
        std::fs::metadata(journal.path()).expect("metadata").len(),
        len
    );
    let e = journal
        .observe(&a, None)
        .expect_err("a find of nothing closes nothing, so it takes no reserve");
    assert_eq!(e.sub, Sub::JournalFull { len });
    journal
        .settle(
            &a,
            &Settled::Created {
                id: 5000,
                url: "https://github.com/fixture-dev/app/releases/tag/v1".to_string(),
            },
        )
        .expect("a settle fits in the reserve");
    assert!(journal.open().expect("open").is_empty());
}

#[test]
fn once_one_intent_is_refused_every_later_one_is() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    let open = begin(&journal, draft(WriteOp::ReleaseCreate, "v1"));
    let closed = begin(&journal, draft(WriteOp::ReleaseCreate, "v0"));
    journal
        .settle(&closed, &Settled::NeverSent)
        .expect("settle");
    // Below the bound, room for a short Intent's frame but not for the largest frame.
    let len = fill_to(&journal, JOURNAL_MAX - JOURNAL_RESERVE - 2000);
    for key in ["v2".to_string(), "k".repeat(3000)] {
        let e = journal
            .begin(&draft(WriteOp::ReleaseCreate, &key))
            .expect_err("no Intent fits, whatever its size");
        assert_eq!(e.sub, Sub::JournalFull { len }, "{} bytes", key.len());
    }
    let e = journal
        .observe(&closed, Some(5000))
        .expect_err("a find of a closed intent closes nothing");
    assert_eq!(e.sub, Sub::JournalFull { len });
    let e = journal
        .observe(&open, None)
        .expect_err("a find of nothing closes nothing");
    assert_eq!(e.sub, Sub::JournalFull { len });
    assert_eq!(
        std::fs::metadata(journal.path()).expect("metadata").len(),
        len
    );
    journal
        .observe(&open, Some(5000))
        .expect("a find that closes its open intent fits in the reserve");
    assert!(journal.open().expect("open").is_empty());
    let e = journal
        .observe(&open, Some(5000))
        .expect_err("its intent is closed now");
    assert_eq!(e.sub.slug().as_deref(), Some("journal_full"));
}

#[test]
fn a_long_created_url_is_journaled_empty() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    let tag = "https://github.com/fixture-dev/app/releases/tag/";
    let long = begin(&journal, draft(WriteOp::ReleaseCreate, "v1"));
    let url = format!("{tag}{}", "%E2%9C%93".repeat(560));
    assert!(url.len() > 5000);
    journal
        .settle(&long, &Settled::Created { id: 5000, url })
        .expect("the outcome of a sent write always fits");
    let view = journal.view(&long).expect("view").expect("held");
    assert_eq!(
        view.settled,
        Some(Settled::Created {
            id: 5000,
            url: String::new()
        })
    );
    assert!(!view.open);

    // One whose JSON text is exactly the bound (its two quotes included) is kept.
    let kept = begin(&journal, draft(WriteOp::ReleaseCreate, "v2"));
    let url = format!("{tag}{}", "a".repeat(URL_MAX - 2 - tag.len()));
    let created = Settled::Created { id: 5001, url };
    journal.settle(&kept, &created).expect("settle");
    let view = journal.view(&kept).expect("view").expect("held");
    assert_eq!(view.settled, Some(created));
}

#[test]
fn a_damaged_journal_raises_no_need() {
    let root = scratch();
    let fx = fx();
    let dirs = Dirs::under(root.path());
    let journal = journal(root.path(), &fx);
    let x = begin(&journal, draft(WriteOp::ReleaseCreate, "v1"));
    let first_end = bytes(&journal).len();
    let w = begin(&journal, draft(WriteOp::ReleaseCreate, "v2"));
    journal
        .settle(
            &x,
            &Settled::Created {
                id: 5000,
                url: "https://github.com/fixture-dev/app/releases/tag/v1".to_string(),
            },
        )
        .expect("settle");
    let whole = bytes(&journal);
    let mut damaged = whole.clone();
    // A byte inside W's body, so the record that closes X lies past the fault.
    damaged[first_end + FRAME_OVERHEAD + 3] ^= 0x01;
    write_raw(&journal, &damaged);
    fx.sleep(SETTLE_WINDOW + Duration::from_secs(1))
        .expect("sleep");
    let (records, end) = journal.read().expect("read");
    assert_eq!(records.len(), 1);
    assert!(matches!(end, JournalEnd::Fault { .. }), "{end:?}");
    assert!(
        need_keys(&dirs).is_empty(),
        "X looks open only before the fault"
    );

    write_raw(&journal, &whole);
    journal.read().expect("read");
    assert_eq!(need_keys(&dirs), [stale_key(&w)]);
}

#[test]
fn retracting_a_closed_or_missing_intent_clears_its_need() {
    let root = scratch();
    let fx = fx();
    let dirs = Dirs::under(root.path());
    let journal = journal(root.path(), &fx);
    let past = SETTLE_WINDOW + Duration::from_secs(1);

    // Closed through another `Journal` value, as another agh closes it: its clear comes after
    // the raise, which ran under the lock, and no later scan raises the need again.
    let x = begin(&journal, draft(WriteOp::ReleaseCreate, "v1"));
    fx.sleep(past).expect("sleep");
    journal.open().expect("open");
    assert_eq!(need_keys(&dirs), [stale_key(&x)]);
    let other = Journal::open(&dirs, &app(), &fx).expect("open");
    other.observe(&x, Some(5000)).expect("observe");
    journal.open().expect("open");
    assert!(need_keys(&dirs).is_empty());

    // A crash between a closing record and its clear leaves the need; its own `then`, the
    // retract, is refused `not_open` and clears it.
    let y = begin(&journal, draft(WriteOp::ReleaseCreate, "v2"));
    fx.sleep(past).expect("sleep");
    journal.open().expect("open");
    journal
        .append(&JournalRecord::Settled {
            intent: y.clone(),
            outcome: Settled::NeverSent,
        })
        .expect("a closing record with no clear after it");
    assert_eq!(need_keys(&dirs), [stale_key(&y)]);
    let e = journal.retract(&y).expect_err("not open");
    assert_eq!(
        e.sub,
        Sub::NotOpen {
            state: "settled".to_string()
        }
    );
    assert!(need_keys(&dirs).is_empty());

    // A journal moved aside: the retract is NotFound and clears the need.
    let z = begin(&journal, draft(WriteOp::ReleaseCreate, "v3"));
    fx.sleep(past).expect("sleep");
    journal.open().expect("open");
    assert_eq!(need_keys(&dirs), [stale_key(&z)]);
    let aside = journal.path().with_extension("steps.aside");
    std::fs::rename(journal.path(), &aside).expect("moved aside");
    let e = journal.retract(&z).expect_err("not held");
    assert_eq!(e.exit(), 6);
    assert!(need_keys(&dirs).is_empty());
    assert!(!journal.path().exists());
}

#[test]
fn a_token_shaped_key_is_refused_not_redacted() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    let token = agh_fake::token("7");
    let by_key = draft(WriteOp::ReleaseCreate, &token);
    let mut by_target = draft(WriteOp::ReleaseCreate, "v1");
    by_target.target_commitish = Some(token.clone());
    for draft in [by_key, by_target] {
        let e = journal.begin(&draft).expect_err("refused");
        assert_eq!(e.exit(), 2);
        let line = e.line("release.create", &Redactor::new());
        assert!(line.contains("shaped like a credential"), "{line}");
        assert!(!line.contains(&token));
    }
    let (records, _) = journal.read().expect("read");
    assert!(records.is_empty());

    // A repository may be named like a token, and the journal writes its slug lowercased:
    // either spelling is refused, never journaled redacted.
    let dirs = Dirs::under(root.path());
    for repo in [format!("{}_tools", "GHS"), format!("{}_tools", "ghs")] {
        let slug = Slug::parse(&format!("fixture-dev/{repo}")).expect("a slug");
        let shaped = Journal::open(&dirs, &slug, &fx).expect("open");
        let mut on_repo = draft(WriteOp::ReleaseCreate, "v1");
        on_repo.slug = slug;
        let e = shaped.begin(&on_repo).expect_err("a token-shaped slug");
        assert_eq!(e.exit(), 2, "{repo}: {}", e.why);
        assert!(e.why.contains("shaped like a credential"), "{}", e.why);
        let (records, end) = shaped.read().expect("read");
        assert!(records.is_empty(), "{repo}");
        assert_eq!(end, JournalEnd::Clean, "{repo}");
    }
    assert!(journals(&dirs).expect("journals").is_empty());
}

#[test]
fn a_record_that_would_not_read_back_is_refused() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    let elsewhere = scratch();
    let source = Journal::open(&Dirs::under(elsewhere.path()), &app(), &fx).expect("open");
    for key in ["a", "b"] {
        begin(&source, draft(WriteOp::ReleaseCreate, key));
    }
    let (records, _) = source.read().expect("read");
    // An Intent whose id names step 1, appended where the next step is 0.
    let JournalRecord::Intent(intent) = &records[1].1 else {
        panic!("an Intent at step 1: {records:?}");
    };
    let e = journal.append(&records[1].1).expect_err("a fault on read");
    assert_eq!(e.exit(), 2);
    assert_eq!(e.kind, Kind::Usage);
    assert!(e.why.contains(intent.id.as_str()), "{}", e.why);
    assert!(!journal.path().exists());
}

#[test]
fn a_record_over_4_kib_is_refused() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    let e = journal
        .begin(&draft(WriteOp::ReleaseCreate, &"k".repeat(5000)))
        .expect_err("too long");
    assert_eq!(e.exit(), 1);
    assert_eq!(e.sub.slug().as_deref(), Some("journal_fault"));
    let (records, _) = journal.read().expect("read");
    assert!(records.is_empty());
}

#[test]
fn no_body_can_nest_a_frame() {
    let mut text: String = (0u8..0x20).map(char::from).collect();
    text.push_str("\u{a5}q");
    text.push_str("\u{a5}q\u{1}\u{0}");
    let first = JournalRecord::Settled {
        intent: IntentId::parse("0-000000000000").expect("an id"),
        outcome: Settled::Unknown { why: text.clone() },
    };
    let second = JournalRecord::Settled {
        intent: IntentId::parse("0-000000000000").expect("an id"),
        outcome: Settled::Created { id: 1, url: text },
    };
    for record in [&first, &second] {
        assert!(encode(record).iter().all(|&b| b >= 0x20));
    }
    let one = astream_wire::steps::encode_step(0, &encode(&first)).expect("a frame");
    let two = astream_wire::steps::encode_step(1, &encode(&second)).expect("a frame");
    let mut whole = one.clone();
    whole.extend_from_slice(&two);

    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    let at = u64::try_from(one.len()).expect("small");
    for cut in one.len() + 1..whole.len() {
        write_raw(&journal, &whole[..cut]);
        let (records, end) = journal.read().expect("read");
        assert_eq!(records.len(), 1, "cut at {cut}");
        assert_eq!(end, JournalEnd::TornRepaired { at }, "cut at {cut}");
    }
}

#[test]
fn intent_store_open_settle_observe_latest() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    let a = begin(&journal, draft(WriteOp::ReleaseCreate, "a"));
    let open_ids = |j: &Journal| -> Vec<IntentId> {
        j.open().expect("open").into_iter().map(|i| i.id).collect()
    };
    assert_eq!(open_ids(&journal), std::slice::from_ref(&a));
    journal
        .settle(
            &a,
            &Settled::Unknown {
                why: "no answer".to_string(),
            },
        )
        .expect("settle");
    journal.observe(&a, None).expect("observe");
    assert_eq!(open_ids(&journal), std::slice::from_ref(&a));
    journal.observe(&a, Some(5000)).expect("observe");
    assert!(open_ids(&journal).is_empty());
    let view = journal.view(&a).expect("view").expect("held");
    assert_eq!(view.observed, [(None, SIM_START), (Some(5000), SIM_START)]);

    let b = begin(&journal, draft(WriteOp::ReleaseCreate, "b"));
    journal
        .settle(&b, &Settled::Refused { status: 422 })
        .expect("settle");
    let c = begin(&journal, draft(WriteOp::ReleaseCreate, "c"));
    journal.settle(&c, &Settled::NeverSent).expect("settle");
    let d = begin(&journal, draft(WriteOp::ReleaseCreate, "v1"));
    let created = Settled::Created {
        id: 5001,
        url: "https://github.com/fixture-dev/app/releases/tag/v1".to_string(),
    };
    journal.settle(&d, &created).expect("settle");
    assert!(open_ids(&journal).is_empty());
    let latest = journal
        .latest(WriteOp::ReleaseCreate, "v1")
        .expect("latest")
        .expect("d");
    assert_eq!(latest.intent.id, d);
    assert_eq!(latest.settled, Some(created));
    assert_eq!(
        journal
            .latest(WriteOp::ReleaseCreate, "v9")
            .expect("latest"),
        None
    );

    let before = bytes(&journal);
    let stranger = IntentId::parse("99-000000000000").expect("an id");
    let e = journal
        .settle(&stranger, &Settled::NeverSent)
        .expect_err("not held");
    assert_eq!(e.kind, Kind::Usage);
    assert_eq!(bytes(&journal), before);
}

#[test]
fn the_settle_window_raises_and_clears_the_need() {
    let root = scratch();
    let fx = fx();
    let dirs = Dirs::under(root.path());
    let journal = journal(root.path(), &fx);
    let id = begin(&journal, draft(WriteOp::ReleaseCreate, "v1"));
    fx.sleep(SETTLE_WINDOW).expect("sleep");
    journal.open().expect("open");
    assert!(needs::list(&dirs).expect("needs").is_empty());

    fx.sleep(Duration::from_secs(1)).expect("sleep");
    journal.open().expect("open");
    let listed = needs::list(&dirs).expect("needs");
    let [need] = listed.as_slice() else {
        panic!("one need: {listed:?}");
    };
    assert_eq!(need.key, format!("agh.intent.{id}.stale"));
    for part in ["release.create", "v1", "fixture-dev/app"] {
        assert!(need.what.contains(part), "{}", need.what);
    }
    assert_eq!(need.then.matches(id.as_str()).count(), 2, "{}", need.then);
    let first_seen = fx.wall_secs().expect("now");
    assert_eq!(need.at_secs, first_seen);
    // Later scans leave the need as it was raised: it is written once, not on every scan.
    fx.sleep(Duration::from_secs(60)).expect("sleep");
    journal.open().expect("open");
    let listed = needs::list(&dirs).expect("needs");
    assert_eq!(
        listed.iter().map(|n| n.at_secs).collect::<Vec<_>>(),
        [first_seen]
    );

    journal.observe(&id, Some(5000)).expect("observe");
    assert!(needs::list(&dirs).expect("needs").is_empty());

    let second = begin(&journal, draft(WriteOp::ReleaseCreate, "v2"));
    fx.sleep(SETTLE_WINDOW + Duration::from_secs(1))
        .expect("sleep");
    journal.open().expect("open");
    assert_eq!(needs::list(&dirs).expect("needs").len(), 1);
    journal.retract(&second).expect("retract");
    assert!(needs::list(&dirs).expect("needs").is_empty());

    // An upload answered after the window, whose need another scan raised meanwhile: the
    // closing settle clears it, and its own scan raises none.
    let late = begin(&journal, draft(WriteOp::ReleaseCreate, "v3"));
    fx.sleep(SETTLE_WINDOW + Duration::from_secs(1))
        .expect("sleep");
    journal.open().expect("open");
    assert_eq!(needs::list(&dirs).expect("needs").len(), 1);
    journal
        .settle(
            &late,
            &Settled::Created {
                id: 5002,
                url: "https://github.com/fixture-dev/app/releases/tag/v3".to_string(),
            },
        )
        .expect("settle");
    assert!(needs::list(&dirs).expect("needs").is_empty());
    journal.open().expect("open");
    assert!(needs::list(&dirs).expect("needs").is_empty());

    // An Unknown leaves the intent open, and its need stands.
    let unknown = begin(&journal, draft(WriteOp::ReleaseCreate, "v4"));
    fx.sleep(SETTLE_WINDOW + Duration::from_secs(1))
        .expect("sleep");
    journal
        .settle(
            &unknown,
            &Settled::Unknown {
                why: "no answer".to_string(),
            },
        )
        .expect("settle");
    let listed = needs::list(&dirs).expect("needs");
    assert_eq!(
        listed.iter().map(|n| n.key.clone()).collect::<Vec<_>>(),
        [format!("agh.intent.{unknown}.stale")]
    );
}

#[test]
fn retract_closes_an_open_intent_once() {
    let root = scratch();
    let fx = fx();
    let journal = journal(root.path(), &fx);
    let id = begin(&journal, draft(WriteOp::ReleaseCreate, "v1"));
    fx.sleep(Duration::from_secs(30)).expect("sleep");
    journal.retract(&id).expect("retract");
    let view = journal.view(&id).expect("view").expect("held");
    assert!(!view.open);
    assert_eq!(view.retracted, Some(SIM_START + 30));
    let e = journal.retract(&id).expect_err("again");
    assert_eq!(e.exit(), 1);
    assert_eq!(
        e.sub,
        Sub::NotOpen {
            state: "retracted".to_string()
        }
    );

    let created = begin(&journal, draft(WriteOp::ReleaseCreate, "v2"));
    journal
        .settle(
            &created,
            &Settled::Created {
                id: 5000,
                url: "https://github.com/fixture-dev/app/releases/tag/v2".to_string(),
            },
        )
        .expect("settle");
    let e = journal.retract(&created).expect_err("settled");
    assert_eq!(
        e.sub,
        Sub::NotOpen {
            state: "settled".to_string()
        }
    );
    let found = begin(&journal, draft(WriteOp::ReleaseCreate, "v3"));
    journal.observe(&found, Some(5001)).expect("observe");
    let e = journal.retract(&found).expect_err("found");
    assert_eq!(
        e.sub,
        Sub::NotOpen {
            state: "found".to_string()
        }
    );
    let e = journal
        .retract(&IntentId::parse("99-000000000000").expect("an id"))
        .expect_err("not held");
    assert_eq!(e.exit(), 6);
}

#[test]
fn journals_lists_every_repository() {
    let root = scratch();
    let fx = fx();
    let dirs = Dirs::under(root.path());
    assert!(journals(&dirs).expect("journals").is_empty());
    for slug in ["fixture-dev/app", "alabsystems/aterm"] {
        let slug = Slug::parse(slug).expect("a slug");
        let journal = Journal::open(&dirs, &slug, &fx).expect("open");
        let mut d = draft(WriteOp::ReleaseCreate, "v1");
        d.slug = slug;
        journal.begin(&d).expect("begin");
    }
    std::fs::write(dirs.journal().join("README"), b"notes").expect("a stray file");
    std::fs::create_dir(dirs.journal().join("x y")).expect("a stray directory");
    std::fs::write(dirs.journal().join("x y").join("z.steps"), b"").expect("a stray journal");
    let listed: Vec<String> = journals(&dirs)
        .expect("journals")
        .into_iter()
        .map(|(slug, _)| slug.key())
        .collect();
    assert_eq!(listed, ["alabsystems/aterm", "fixture-dev/app"]);
}
