// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::bridge::ACK_DEADLINE;

#[test]
fn follow_handoff_buffers_no_row_for_a_stalled_executor() {
    let (tx, _rx) = notice_handoff();
    let row = Notice::Record(0, 41, String::from("first"), vec![1; 1024]);
    assert!(
        matches!(tx.try_send(row), Err(mpsc::TrySendError::Full(_))),
        "a stalled executor must leave no queued row"
    );
}

#[test]
fn a_subject_segment_that_is_not_a_principal_never_becomes_an_environment_value() {
    let huge = "x".repeat(200_000);
    let bidi = "s-\u{202e}gnitset";

    let ask = Sel::Ask("h-andrew".into());
    assert!(ask
        .hit(
            "f1",
            7,
            "/f/f1/in/p/h-andrew/n-a/ask",
            b"v=1 t=1 from=tzzzzzzzzzb text=hi"
        )
        .is_some());
    for bad in [
        format!("/f/f1/in/p/h-andrew/{huge}/ask"),
        format!("/f/f1/in/p/h-andrew/{bidi}/ask"),
        "/f/f1/in/p/h-andrew/NODE/ask".to_string(),
    ] {
        assert_eq!(ask.hit("f1", 7, &bad, b"v=1 t=1"), None, "{}", &bad[..40]);
    }
    for from in [huge.as_str(), bidi, "h-b"] {
        let body = format!("v=1 t=1 from={from} text=hi");
        let hit = ask
            .hit("f1", 7, "/f/f1/in/p/h-andrew/n-a/ask", body.as_bytes())
            .expect("the subject is honest");
        assert_eq!(hit.sid, "", "{}", &from[..from.len().min(40)]);
    }

    let att = Sel::Attention;
    let body = b"v=1 t=1 state=live attention=needs%20a%20key";
    assert!(att
        .hit("f1", 3, "/f/f1/pub/n-a/tzzzzzzzzzb/presence", body)
        .is_some());
    for bad in [
        format!("/f/f1/pub/n-a/{huge}/presence"),
        format!("/f/f1/pub/{huge}/tzzzzzzzzzb/presence"),
        format!("/f/f1/pub/n-a/{bidi}/presence"),
    ] {
        assert_eq!(att.hit("f1", 3, &bad, body), None, "{}", &bad[..40]);
    }

    let halt = Sel::Halt;
    let on = b"v=1 t=1 state=on reason=main%20broken";
    assert!(halt.hit("f1", 4, "/f/f1/fleet/h-andrew/halt", on).is_some());
    assert_eq!(
        halt.hit("f1", 4, &format!("/f/f1/fleet/{huge}/halt"), on),
        None
    );

    let hit = ask
        .hit(
            "f1",
            7,
            "/f/f1/in/p/h-andrew/n-a/ask",
            b"v=1 t=1 from=tzzzzzzzzzb text=hi",
        )
        .expect("the honest ask");
    for v in [&hit.subject, &hit.node, &hit.who, &hit.sid, &hit.text] {
        assert!(v.len() <= TEXT_MAX, "{v} is {} bytes", v.len());
        assert!(
            v.chars().all(|c| c.is_ascii_graphic() || c == ' '),
            "{v} is not printable ASCII"
        );
    }
}

#[derive(Clone)]
struct Scratch {
    _dir: std::sync::Arc<aterm_tempfile::TempDir>,
    path: String,
}

impl std::ops::Deref for Scratch {
    type Target = str;

    fn deref(&self) -> &str {
        &self.path
    }
}

impl AsRef<str> for Scratch {
    fn as_ref(&self) -> &str {
        &self.path
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        Path::new(&self.path)
    }
}

impl AsRef<std::ffi::OsStr> for Scratch {
    fn as_ref(&self) -> &std::ffi::OsStr {
        std::ffi::OsStr::new(&self.path)
    }
}

impl std::fmt::Display for Scratch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.path.fmt(f)
    }
}

/// A fresh scratch dir for `tag`. Its prefix is short because a broker socket a test binds in
/// it must fit `sun_path` ([`crate::enable::SUN_PATH_MAX`] bytes, its NUL included). Under the
/// build shim's `$TMPDIR/.ubd/` with a five-digit pid, the old `atlink-notify-` prefix made
/// `need-derived-late-false/b.sock` a path of 104 or 105 bytes, past the 103 that fit.
fn scratch(tag: &str) -> Scratch {
    let dir = std::sync::Arc::new(crate::test_scratch_dir(tag));
    let path = dir.path().join("s").to_string_lossy().into_owned();
    Scratch { _dir: dir, path }
}

fn cfg(dir: &str) -> Config {
    Config {
        fleet: "f1".into(),
        broker: String::new(),
        cap_files: Vec::new(),
        state_dir: dir.to_string(),
        on: vec![Sel::Attention],
        exec: "true".into(),
        limit: 2,
        window_ms: 60_000,
        since: Since::Head,
        once: true,
        exec_timeout: aterm_spec::wait::MUST_HAPPEN,
        bridge_state: None,
    }
}

#[test]
fn a_selector_matches_by_position_and_an_overlong_subject_never_matches() {
    let ask = Sel::Ask("h-andrew".into());
    let honest = "/f/f1/in/p/h-andrew/n-a/ask";
    let hit = ask
        .hit(
            "f1",
            7,
            honest,
            b"v=1 t=1 from=tzzzzzzzzzb text=is%20this%20ok%3F",
        )
        .expect("the honest ask to h-andrew matches");
    assert_eq!(
        (hit.who.as_str(), hit.node.as_str(), hit.sid.as_str()),
        ("h-andrew", "n-a", "tzzzzzzzzzb")
    );
    assert_eq!(hit.text, "is this ok?");
    for bad in [
        "/f/f1/in/n-a/tzzzzzzzzzb/h-andrew/ask",
        "/f/f1/in/p/h-andrew/n-a/t0000000001/ask",
        "/f/f1/in/p/h-other/n-a/ask",
        "/f/f1/in/p/h-andrew/n-a/note",
        "/f/f2/in/p/h-andrew/n-a/ask",
        "/f/f1/pub/p/h-andrew/n-a/ask",
        "/f/f1/in/n-a/h-andrew/n-b/ask",
    ] {
        assert_eq!(ask.hit("f1", 7, bad, b"v=1 t=1"), None, "{bad}");
    }
}

#[test]
fn attention_fires_only_on_a_presence_row_that_carries_one() {
    let sel = Sel::Attention;
    let subject = "/f/f1/pub/n-a/tzzzzzzzzzb/presence";
    let hit = sel
        .hit(
            "f1",
            3,
            subject,
            b"v=1 t=1 state=live attention=needs%20a%20key",
        )
        .expect("a presence row with an attention matches");
    assert_eq!(hit.text, "needs a key");
    assert_eq!(
        (hit.node.as_str(), hit.who.as_str()),
        ("n-a", "tzzzzzzzzzb")
    );
    for body in [
        &b"v=1 t=1 state=live"[..],
        b"v=1 t=1 attention=",
        b"v=1 t=1 attention=-",
    ] {
        assert_eq!(sel.hit("f1", 3, subject, body), None);
    }
    let halt = Sel::Halt;
    let lane = "/f/f1/fleet/h-andrew/halt";
    assert!(halt
        .hit("f1", 4, lane, b"v=1 t=1 state=on reason=main%20broken")
        .is_some());
    assert_eq!(halt.hit("f1", 4, lane, b"v=1 t=1 state=off"), None);
    for lane in ["/f/f1/fleet/a-svc/halt", "/f/f1/fleet/n-node/halt"] {
        assert_eq!(
            halt.hit("f1", 4, lane, b"v=1 t=1 state=on reason=stop"),
            None,
            "{lane}"
        );
    }
}

#[test]
fn untrusted_text_is_flattened_and_capped() {
    let text = one_line("hi\x1b[2Jthere\nand\ta tab");
    assert_eq!(text, "hi.[2Jthere.and.a tab");
    assert_eq!(one_line(&"x".repeat(400)).len(), TEXT_MAX);
}

#[test]
fn the_budget_is_a_sliding_window_over_the_stamps_it_persisted() {
    let dir = scratch("budget");
    let mut n = Notifier::new(cfg(&dir)).expect("open");
    assert!(n.has_room(1_000));
    n.charge(1_000).expect("charge");
    assert!(n.has_room(1_100));
    n.charge(1_100).expect("charge");
    assert!(!n.has_room(1_200), "two in the window is the limit of 2");
    assert!(n.has_room(61_050), "the 1_000 stamp has left the window");
    drop(n);
    let mut back = Notifier::new(cfg(&dir)).expect("reopen");
    assert!(
        !back.has_room(1_200),
        "a restart does not hand the budget back"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// ONE NOTIFIER PER STORE ([`Store::open`]): while one notifier holds a state dir, a second
/// one's start on it is refused, naming the holder's pid, and writes nothing; once the first
/// lets go, a start takes it.
#[test]
fn a_second_notifier_on_the_same_store_is_refused_at_its_start() {
    let dir = scratch("one-notifier");
    let mut first = Notifier::new(cfg(&dir)).expect("open");
    first.charge(1_000).expect("charge");
    let journal = std::fs::read(first.store.root.join(journal::FILE)).expect("the journal");
    let second = Notifier::new(cfg(&dir));
    let refusal = second
        .err()
        .expect("a second notifier on the same store is refused");
    assert_eq!(refusal.kind(), io::ErrorKind::WouldBlock, "{refusal}");
    let said = refusal.to_string();
    assert!(
        said.contains("held by another `aterm link notify`")
            && said.contains(&format!("(pid {})", std::process::id())),
        "the refusal names the holder: {said}"
    );
    assert_eq!(
        std::fs::read(first.store.root.join(journal::FILE)).expect("the journal"),
        journal,
        "the refused start wrote nothing"
    );
    drop(first);
    let back = Notifier::new(cfg(&dir)).expect("once the first lets go, a start takes it");
    assert_eq!(back.stamps, [1_000]);
}

/// Renaming the held directory ends every writer, including readers reconnecting later.
#[test]
fn a_notifier_whose_store_is_renamed_stops_and_keeps_its_loss() {
    let dir = scratch("directory-moved");
    let c = cfg(&dir);
    let mut first = Notifier::new(c.clone()).expect("open");
    first.charge(1_000).expect("charge");
    let root = first.store.root.clone();
    let aside = root.with_extension("aside");
    std::fs::rename(&root, &aside).expect("rename held root");
    let lost = first
        .charge(2_000)
        .expect_err("renamed root refuses writes");
    assert_eq!(lost.kind(), io::ErrorKind::NotFound);
    let mut second = Notifier::new(c.clone()).expect("new physical directory");
    second.charge(5_000).expect("new owner writes");
    let journal = std::fs::read(root.join(journal::FILE)).unwrap();
    let again = first.charge(3_000).expect_err("loss is sticky");
    assert_eq!(again.to_string(), lost.to_string());
    let log = ReadLog {
        id: Some(astream_broker::LogIdentity {
            id: [7; 16],
            generation: 0,
        }),
        lineage: None,
        proved: Steps::default(),
        ordered: false,
        carried: 0,
    };
    assert_eq!(
        reader_store(&first.hold, &log)
            .expect_err("reader also refuses")
            .to_string(),
        lost.to_string()
    );
    assert_eq!(std::fs::read(root.join(journal::FILE)).unwrap(), journal);
    assert!(!root.join("log").exists());
    drop(second);
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::rename(&aside, &root).unwrap();
    assert_eq!(
        first
            .charge(4_000)
            .expect_err("restoring the path cannot clear loss")
            .to_string(),
        lost.to_string()
    );
    drop(first);
    let back = Notifier::new(c).expect("original owner retired");
    assert_eq!(back.stamps, [1_000]);
}

/// An unreadable path is Unknown, not evidence that directory ownership was lost.
#[test]
fn a_store_check_that_cannot_read_its_path_records_no_loss() {
    let dir = scratch("directory-errno");
    let mut store = Store::open(&dir).expect("open");
    let original = store.root.clone();
    let probe = original.join("loop");
    std::os::unix::fs::symlink("loop", &probe).unwrap();
    Arc::get_mut(&mut store.access.0)
        .expect("sole owner")
        .lock
        .path = probe.join("missing");
    let errno = store.access.0.check().expect_err("unreadable path");
    assert!(errno.raw_os_error().is_some());
    assert!(store.access.0.lost().is_none());
    Arc::get_mut(&mut store.access.0).unwrap().lock.path = original.clone();
    store.access.0.check().expect("same original directory");
    let aside = original.with_extension("aside");
    std::fs::rename(&original, &aside).unwrap();
    let gone = store.access.0.check().expect_err("positive absence");
    std::fs::rename(&aside, &original).unwrap();
    assert_eq!(
        store.access.0.check().expect_err("sticky loss").to_string(),
        gone.to_string()
    );
}

/// The PID file is diagnostic DATA; unlinking it cannot release the directory owner.
#[test]
fn removing_the_pid_file_does_not_release_the_directory_owner() {
    let dir = scratch("directory-pid");
    let mut first = Notifier::new(cfg(&dir)).unwrap();
    std::fs::remove_file(first.store.root.join(LOCK)).unwrap();
    let refused = Notifier::new(cfg(&dir))
        .err()
        .expect("directory still owned");
    assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
    first
        .charge(1_000)
        .expect("PID file absence is not ownership loss");
    drop(first);
    assert_eq!(Notifier::new(cfg(&dir)).unwrap().stamps, [1_000]);
}

#[test]
fn a_store_whose_file_system_takes_no_lock_is_refused() {
    let dir = scratch("directory-unsupported");
    let store = Store::open(&dir).unwrap();
    let judge = |taken| Store::judge_take_at(&store.root, &store.directory, taken);
    let unsupported = judge(Err(std::fs::TryLockError::Error(io::Error::from(
        io::ErrorKind::Unsupported,
    ))))
    .expect_err("no lock is no store");
    assert_eq!(unsupported.kind(), io::ErrorKind::Unsupported);
    assert!(unsupported
        .to_string()
        .contains("--state on a volume that locks"));
    judge(Ok(())).expect("actual take result");
    let other = judge(Err(std::fs::TryLockError::Error(
        io::Error::from_raw_os_error(libc::EIO),
    )))
    .expect_err("other errno preserved");
    assert_eq!(other.raw_os_error(), Some(libc::EIO));
}

fn appending(dir: &str, limit: u32, exec: &str) -> (Notifier, PathBuf) {
    std::fs::create_dir_all(dir).expect("scratch");
    let fired = Path::new(dir).join("fired");
    let mut c = cfg(dir);
    c.limit = limit;
    c.exec = format!(
        "{exec}printf '%s\\n' \"$ATERM_NOTIFY_OFFSET\" >> {}",
        fired.display()
    );
    (Notifier::new(c).expect("open"), fired)
}

fn fired_offsets(fired: &Path) -> Vec<u64> {
    std::fs::read_to_string(fired)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

fn generated_sid(index: u64) -> String {
    assert!(
        index < (1u64 << aterm_types::tab_id::BITS),
        "a distinct bounded fixture index"
    );
    aterm_types::tab_id::TabId::from_bits(index)
        .as_str()
        .to_owned()
}

fn feed(n: &mut Notifier, off: u64, sid: &str, body: &str) {
    let subject = format!("/f/f1/pub/n-a/{sid}/presence");
    n.on_record(&Sel::Attention, off, &subject, body.as_bytes())
        .expect("on_record");
}

#[test]
fn a_standing_attention_pages_once_per_transition_not_once_per_offset() {
    let dir = scratch("standing");
    let (mut n, fired) = appending(&dir, 100, "");
    let key = "attention=needs%20a%20key";
    feed(
        &mut n,
        1,
        "tzzzzzzzzz1",
        &format!("v=1 t=1 state=live inc=1 {key}"),
    );
    feed(
        &mut n,
        2,
        "tzzzzzzzzz1",
        &format!("v=1 t=2 state=live inc=2 {key}"),
    );
    feed(
        &mut n,
        3,
        "tzzzzzzzzz1",
        &format!("v=1 t=3 inc=2 phase=busy {key}"),
    );
    feed(
        &mut n,
        4,
        "tzzzzzzzzz1",
        &format!("v=1 t=4 state=exited inc=2 {key}"),
    );
    feed(
        &mut n,
        5,
        "tzzzzzzzzz2",
        &format!("v=1 t=5 state=live inc=1 {key}"),
    );
    assert_eq!(fired_offsets(&fired), [1, 5]);
    assert_eq!((n.counts.fired, n.counts.standing), (2, 3));
    for off in [2, 3, 4] {
        assert_eq!(n.verdict(off), None, "a republish is not a verdict");
    }
    assert_eq!(n.stamps.len(), 2, "and charges no budget");

    feed(
        &mut n,
        6,
        "tzzzzzzzzz1",
        "v=1 t=6 state=live inc=3 attention=-",
    );
    feed(
        &mut n,
        7,
        "tzzzzzzzzz1",
        &format!("v=1 t=7 state=live inc=3 {key}"),
    );
    assert_eq!(fired_offsets(&fired), [1, 5, 7], "a clear re-arms it");

    drop(n);
    let (mut back, _) = appending(&dir, 100, "");
    feed(
        &mut back,
        8,
        "tzzzzzzzzz1",
        &format!("v=1 t=8 state=live inc=4 {key}"),
    );
    assert_eq!(fired_offsets(&fired), [1, 5, 7], "a restart remembers");
    feed(
        &mut back,
        9,
        "tzzzzzzzzz1",
        "v=1 t=9 attention=a%20new%20question",
    );
    feed(&mut back, 10, "tzzzzzzzzz1", &format!("v=1 t=10 {key}"));
    assert_eq!(
        fired_offsets(&fired),
        [1, 5, 7, 9, 10],
        "a value that moved is a new escalation, and so is moving back"
    );

    feed(&mut back, 11, "tzzzzzzzzz2", "v=1 t=11 attention=-");
    feed(&mut back, 12, "tzzzzzzzzz2", &format!("v=1 t=12 {key}"));
    feed(&mut back, 11, "tzzzzzzzzz2", "v=1 t=11 attention=-");
    feed(&mut back, 12, "tzzzzzzzzz2", &format!("v=1 t=12 {key}"));
    feed(
        &mut back,
        13,
        "tzzzzzzzzz2",
        &format!("v=1 t=13 inc=2 {key}"),
    );
    assert_eq!(fired_offsets(&fired), [1, 5, 7, 9, 10, 12]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_escalation_nobody_was_told_of_is_offered_again_on_its_republish() {
    let dir = scratch("untold");
    let (mut n, fired) = appending(&dir, 1, "");
    feed(&mut n, 1, "tzzzzzzzzz1", "v=1 t=1 attention=first");
    feed(&mut n, 2, "tzzzzzzzzz2", "v=1 t=2 attention=second");
    assert_eq!(n.verdict(2), Some("dropped"));
    drop(n);
    let (mut room, _) = appending(&dir, 10, "");
    feed(
        &mut room,
        3,
        "tzzzzzzzzz2",
        "v=1 t=3 inc=2 attention=second",
    );
    assert_eq!(
        fired_offsets(&fired),
        [1, 3],
        "the dropped escalation pages"
    );
    feed(
        &mut room,
        4,
        "tzzzzzzzzz2",
        "v=1 t=4 inc=3 attention=second",
    );
    assert_eq!(fired_offsets(&fired), [1, 3], "and, told, stands");

    drop(room);
    let (mut broken, _) = appending(
        &dir,
        10,
        &format!("if [ ! -e {dir}/fixed ]; then exit 3; fi; "),
    );
    feed(&mut broken, 5, "tzzzzzzzzz3", "v=1 t=5 attention=third");
    feed(
        &mut broken,
        6,
        "tzzzzzzzzz3",
        "v=1 t=6 inc=2 attention=third",
    );
    assert!(
        broken
            .journal
            .iter()
            .any(|l| l.starts_with("6 fired on=attention rc=3")),
        "an untold escalation runs the command on its republish: {:?}",
        broken.journal
    );
    std::fs::write(Path::new(&dir).join("fixed"), b"").expect("fix the command");
    feed(
        &mut broken,
        7,
        "tzzzzzzzzz3",
        "v=1 t=7 inc=3 attention=third",
    );
    feed(
        &mut broken,
        8,
        "tzzzzzzzzz3",
        "v=1 t=8 inc=4 attention=third",
    );
    assert_eq!(fired_offsets(&fired), [1, 3, 7]);
    assert_eq!(broken.counts.standing, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_journal_keeps_one_verdict_per_offset_and_is_bounded() {
    let dir = scratch("journal");
    let mut n = Notifier::new(cfg(&dir)).expect("open");
    n.record(5, "5 firing on=attention".into());
    assert_eq!(n.verdict(5), Some("firing"));
    n.record(5, "5 fired on=attention rc=0 dup=0".into());
    assert_eq!(n.verdict(5), Some("fired"), "the firing line is replaced");
    assert_eq!(n.journal.len(), 1);
    drop(n);
    let mut n = Notifier::new(cfg(&dir)).expect("reopen");
    assert_eq!(n.verdict(5), Some("fired"));
    n.journal.extend(
        (100..99 + JOURNAL_KEEP as u64).map(|off| format!("{off} fired on=attention rc=0 dup=0")),
    );
    for off in 5_000..5_008 {
        n.record(off, format!("{off} fired on=attention rc=0 dup=0"));
    }
    assert_eq!(n.journal.len(), JOURNAL_KEEP);
    assert_eq!(n.verdict(5), None, "the oldest verdicts are forgotten");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn an_uncertain_notify_log_identity_never_claims_another_logs_cursors() {
    let a = astream_broker::LogIdentity {
        id: [0xaa; 16],
        generation: 0,
    };
    let b = astream_broker::LogIdentity {
        id: [0xbb; 16],
        generation: 0,
    };
    for bad in ["invalid-utf8", "malformed", "dangling"] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("notify-owner")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .unwrap();
        let store = Store::open(dir.path().to_str().unwrap()).unwrap();
        store.set_cursor("attention", 900).unwrap();
        store
            .set_journal(&["7 fired".to_string()], Flush::Ordered)
            .unwrap();
        let path = store.root.join("log");
        match bad {
            "invalid-utf8" => std::fs::write(&path, [0xff]).unwrap(),
            "malformed" => std::fs::write(&path, "not-a-log\n").unwrap(),
            "dangling" => std::os::unix::fs::symlink("missing-owner", &path).unwrap(),
            _ => unreachable!(),
        }
        assert!(
            store.for_log(Some(b), None, &Steps::default()).is_err(),
            "{bad}: uncertainty is not permission to adopt A's offsets as B's"
        );
        assert_eq!(store.cursor("attention"), Some(900));
        assert_eq!(store.journal(), ["7 fired"]);
        match bad {
            "invalid-utf8" => assert_eq!(std::fs::read(&path).unwrap(), [0xff]),
            "malformed" => assert_eq!(std::fs::read_to_string(&path).unwrap(), "not-a-log\n"),
            "dangling" => assert_eq!(
                std::fs::read_link(&path).unwrap(),
                PathBuf::from("missing-owner")
            ),
            _ => unreachable!(),
        }
        std::fs::remove_file(&path).unwrap();
        store.write("log", &a.to_string(), Flush::Ordered).unwrap();
        let resumed = store.for_log(Some(a), None, &Steps::default()).unwrap();
        assert_eq!(resumed.cursor("attention"), Some(900));
        assert_eq!(resumed.journal(), ["7 fired"]);
        let fresh = store.for_log(Some(b), None, &Steps::default()).unwrap();
        assert_eq!(fresh.cursor("attention"), None);
        assert!(fresh.journal().is_empty());
    }
}

#[test]
fn only_the_newest_other_logs_stores_are_kept() {
    let id = |n: u8| astream_broker::LogIdentity {
        id: [n; 16],
        generation: 0,
    };
    let dir = aterm_tempfile::Builder::new()
        .prefix("notify-prune")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .unwrap();
    let store = Store::open(dir.path().to_str().unwrap()).unwrap();
    store
        .for_log(Some(id(0)), None, &Steps::default())
        .unwrap()
        .set_cursor("attention", 900)
        .unwrap();
    let logs = store.root.join("logs");
    std::fs::create_dir_all(logs.join("not-an-identity")).unwrap();
    std::fs::create_dir_all(logs.join(id(99).to_string()).join("cur")).unwrap();
    let kept = || {
        let mut names: Vec<String> = std::fs::read_dir(&logs)
            .unwrap()
            .filter_map(|e| e.unwrap().file_name().into_string().ok())
            .filter(|n| astream_broker::LogIdentity::parse(n).is_some())
            .collect();
        names.sort();
        names
    };
    let named = |ns: &[u8]| ns.iter().map(|n| id(*n).to_string()).collect::<Vec<_>>();
    for n in 1..=6 {
        store
            .for_log(Some(id(n)), None, &Steps::default())
            .unwrap()
            .set_cursor("attention", u64::from(n))
            .unwrap();
    }
    assert_eq!(kept(), named(&[3, 4, 5, 6]), "the newest {LOGS_KEEP}");
    assert_eq!(
        store
            .for_log(Some(id(3)), None, &Steps::default())
            .unwrap()
            .cursor("attention"),
        Some(3),
        "a kept log resumes"
    );
    store.for_log(Some(id(7)), None, &Steps::default()).unwrap();
    assert_eq!(kept(), named(&[3, 5, 6, 7]), "3 was read again, 4 was not");
    assert_eq!(
        store
            .for_log(Some(id(1)), None, &Steps::default())
            .unwrap()
            .cursor("attention"),
        None,
        "a pruned log comes back as a first run"
    );
    assert_eq!(
        store
            .for_log(Some(id(0)), None, &Steps::default())
            .unwrap()
            .cursor("attention"),
        Some(900),
        "the root's store stands"
    );
    assert!(logs.join("not-an-identity").is_dir());
}

#[cfg(unix)]
#[test]
fn a_notifier_files_its_cursor_and_journal_under_the_log_it_read() {
    let dir = scratch("logs");
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let serve = |name: &str| {
        let broker = crate::test_broker(base.join(format!("{name}.log"))).unwrap();
        let sock = base.join(format!("{name}.sock"));
        let handle = broker.serve(&sock).unwrap();
        let (mut c, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
        let (off, _) = c
            .publish(
                7,
                1,
                "/f/f1/pub/n-a/tzzzzzzzzzx/presence",
                b"v=1 t=1 attention=help",
            )
            .unwrap();
        assert_eq!(off, 0);
        (sock.to_string_lossy().into_owned(), broker, handle)
    };
    let (a, broker_a, mut server_a) = serve("a");
    let (b, broker_b, mut server_b) = serve("b");
    let fired = base.join("fired");
    let mut c = cfg(&dir);
    c.since = Since::Start;
    c.limit = 10;
    c.exec = format!(
        "printf '%s\\n' \"$ATERM_NOTIFY_OFFSET\" >> {}",
        fired.display()
    );
    let run = |broker: &str| {
        let mut c = c.clone();
        c.broker = broker.to_string();
        Notifier::new(c)
            .expect("open")
            .catch_up()
            .expect("catch up");
        std::fs::read_to_string(&fired)
            .unwrap_or_default()
            .lines()
            .count()
    };
    assert_eq!(run(&a), 1, "A's escalation fires");
    assert_eq!(
        run(&b),
        2,
        "B's escalation is B's, whatever A's cursor says"
    );
    assert_eq!(run(&a), 2, "back on A, nothing fires twice");
    assert_eq!(run(&b), 2, "nor on B");
    let root = base.join("notify");
    assert_eq!(
        std::fs::read_to_string(root.join("log")).unwrap().trim(),
        broker_a.log_identity().to_string(),
        "the root is the first log's"
    );
    assert!(Store::<ReadOnly>::existing(
        root.join("logs").join(broker_b.log_identity().to_string())
    )
    .unwrap()
    .read_cursor("attention")
    .is_some());
    server_a.shutdown();
    server_b.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_catch_up_below_the_retained_floor_reads_on_from_it() {
    let dir = scratch("kept");
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let broker = crate::test_broker(base.join("b.log")).unwrap();
    let sock = base.join("b.sock");
    let mut server = broker.serve(&sock).unwrap();
    let (mut conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
    for (seq, sid) in [(1, "tzzzzzzzzza"), (2, "tzzzzzzzzzb"), (3, "tzzzzzzzzzc")] {
        let subject = format!("/f/f1/pub/n-a/{sid}/presence");
        let (off, _) = conn
            .publish(7, seq, &subject, b"v=1 t=1 attention=help")
            .unwrap();
        assert_eq!(off, seq - 1);
    }
    broker.retain_before_carrying_state(1).expect("the cut");
    let fired = base.join("fired");
    let mut c = cfg(&dir);
    c.broker = sock.to_string_lossy().into_owned();
    c.since = Since::Start;
    c.limit = 10;
    c.exec = format!(
        "printf '%s\\n' \"$ATERM_NOTIFY_OFFSET\" >> {}",
        fired.display()
    );
    Notifier::new(c)
        .expect("open")
        .catch_up()
        .expect("the catch-up reads on from the floor");
    let offsets = std::fs::read_to_string(&fired).unwrap_or_default();
    assert_eq!(
        offsets.lines().collect::<Vec<_>>(),
        ["1", "2"],
        "the escalations the broker kept fire"
    );
    server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum Ended {
    Clean,
    Killed,
    PowerLoss,
}

#[cfg(unix)]
fn after_a_relaxed_stop(
    tag: &str,
    since: Since,
    ended: Ended,
    boot: u8,
    forget: bool,
) -> (u64, Vec<String>) {
    use crate::durability::{self, Durability};
    let dir = scratch(tag);
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let log = base.join("r.log");
    let sock = base.join("r.sock");
    let fired = base.join("fired");
    let mut c = cfg(&dir);
    c.broker = sock.to_string_lossy().into_owned();
    c.since = since;
    c.limit = 10;
    c.exec = format!(
        "printf '%s %s\\n' \"$ATERM_NOTIFY_OFFSET\" \"$ATERM_NOTIFY_TEXT\" >> {}",
        fired.display()
    );
    let escalate = |text: &str| {
        let (mut conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
        let seq = u64::from(text.as_bytes()[text.len() - 1] - b'0') + 1;
        let body = format!("v=1 t=1 attention={text}");
        conn.publish(
            7,
            seq,
            "/f/f1/pub/n-a/tzzzzzzzzzx/presence",
            body.as_bytes(),
        )
        .unwrap()
        .0
    };
    let run = || {
        Notifier::new(c.clone())
            .expect("open")
            .catch_up()
            .expect("catch up");
        std::fs::read_to_string(&fired).unwrap_or_default()
    };
    let (before, cut) = {
        let opened = durability::open(
            &log,
            None,
            Durability::Relaxed,
            Some(durability::test_boot(1)),
        )
        .unwrap();
        if !matches!(ended, Ended::Clean) {
            opened.broker.arm_unclean_stop();
        }
        let mut server = opened.broker.serve(&sock).unwrap();
        assert_eq!(run(), "", "an empty log: the cursor is claimed at 0");
        assert_eq!(escalate("help-0"), 0);
        let cut = std::fs::metadata(&log).unwrap().len();
        for text in ["help-1", "help-2", "help-3"] {
            escalate(text);
        }
        assert_eq!(run().lines().count(), 4, "all four fire in boot 1");
        server.shutdown();
        (opened.broker.log_identity(), cut)
    };
    if matches!(ended, Ended::PowerLoss) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&log)
            .unwrap()
            .set_len(cut)
            .unwrap();
    }
    let reopened = durability::open(
        &log,
        None,
        Durability::Relaxed,
        Some(durability::test_boot(boot)),
    )
    .unwrap();
    let now = reopened.broker.log_identity();
    let moved = now != before;
    assert_eq!(
        now.id, before.id,
        "{tag}: the same log, whatever its generation"
    );
    let mut server = reopened.broker.serve(&sock).unwrap();
    let off = escalate("help-9");
    FORGET_LINEAGE.with(|f| f.set(forget));
    let after = run().lines().skip(4).map(str::to_string).collect();
    FORGET_LINEAGE.with(|f| f.set(false));
    server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        moved,
        boot != 1 && !matches!(ended, Ended::Clean),
        "{tag}: only an unclean stop and a reboot move the generation"
    );
    (off, after)
}

#[cfg(unix)]
#[test]
fn a_power_loss_under_a_relaxed_broker_resumes_the_notifier_where_the_log_continues() {
    for since in [Since::Head, Since::Start] {
        let tag = format!("pl-{since:?}");
        let (off, fired) = after_a_relaxed_stop(&tag, since, Ended::PowerLoss, 2, false);
        assert_eq!(off, 1, "{tag}: the new escalation reuses offset 1");
        assert_eq!(
            fired,
            ["1 help-9"],
            "{tag}: carried over, clamped at the fork"
        );
    }

    let (off, fired) = after_a_relaxed_stop("pl-forget", Since::Head, Ended::PowerLoss, 2, true);
    assert_eq!(off, 1);
    assert!(
        fired.is_empty(),
        "CONTROL: a first run starts past it: {fired:?}"
    );
    let (_, fired) = after_a_relaxed_stop("pl-forget-0", Since::Start, Ended::PowerLoss, 2, true);
    assert_eq!(
        fired,
        ["0 help-0", "1 help-9"],
        "CONTROL: re-read from the start"
    );

    let (off, fired) = after_a_relaxed_stop("pl-ctl", Since::Start, Ended::PowerLoss, 1, false);
    assert_eq!(off, 1);
    assert!(
        fired.is_empty(),
        "CONTROL: the kept identity swallows it: {fired:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_clean_stop_or_a_kill_keeps_a_default_notifiers_place_across_a_reboot() {
    let (off, fired) = after_a_relaxed_stop("clean", Since::Head, Ended::Clean, 2, false);
    assert_eq!(off, 4, "nothing was lost");
    assert_eq!(fired, ["4 help-9"]);

    let (off, fired) = after_a_relaxed_stop("killed", Since::Head, Ended::Killed, 2, false);
    assert_eq!(off, 4, "nothing was lost");
    assert_eq!(fired, ["4 help-9"], "the moved generation continues at 4");

    let (off, fired) = after_a_relaxed_stop("killed-ctl", Since::Head, Ended::Killed, 2, true);
    assert_eq!(off, 4);
    assert!(fired.is_empty(), "CONTROL: read from its head: {fired:?}");
}

#[test]
fn a_moved_generation_carries_the_newest_store_it_continues() {
    let dir = scratch("carry");
    let store = Store::open(&dir).expect("open");
    let g = |generation| astream_broker::LogIdentity {
        id: [7; 16],
        generation,
    };
    let root = store.for_log(Some(g(0)), None, &Steps::default()).unwrap();
    root.set_cursor("attention", 4).unwrap();
    root.set_journal(&["1 fired rc=0".to_string()], Flush::Ordered)
        .unwrap();
    let one = store.for_log(Some(g(1)), None, &Steps::default()).unwrap();
    assert_ne!(one.root, root.root);
    assert_eq!(one.cursor("attention"), None, "no lineage: a first run");
    one.set_cursor("attention", 9).unwrap();
    one.set_journal(
        &[
            "2 fired rc=0".to_string(),
            "6 fired rc=0".to_string(),
            "junk".into(),
        ],
        Flush::Ordered,
    )
    .unwrap();
    one.set_standing(&[("/f/f1/pub/n-a/s-x/presence".into(), "help".into())])
        .unwrap();

    let lineage = lineage_of(&[(1, 3), (2, 5)]);
    let two = store
        .for_log(Some(g(2)), Some(&lineage), &Steps::default())
        .unwrap();
    assert_eq!(
        two.cursor("attention"),
        Some(5),
        "from (id, 1), at its fork"
    );
    assert_eq!(
        two.journal(),
        ["2 fired rc=0"],
        "the verdicts below the fork"
    );
    assert_eq!(two.standing(), one.standing());
    two.set_cursor("attention", 7).unwrap();
    let again = store
        .for_log(Some(g(2)), Some(&lineage), &Steps::default())
        .unwrap();
    assert_eq!(
        again.cursor("attention"),
        Some(7),
        "carried once, then its own"
    );
    let leftovers: Vec<_> = std::fs::read_dir(PathBuf::from(&dir).join("notify/logs"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(".carry-"))
        .collect();
    assert!(leftovers.is_empty(), "no scratch left behind");

    let gap = lineage_of(&[(3, 4)]);
    let three = store
        .for_log(Some(g(4)), Some(&gap), &Steps::default())
        .unwrap();
    assert_eq!(
        three.cursor("attention"),
        None,
        "a step missing: a first run"
    );
    let other = astream_broker::LogIdentity {
        id: [8; 16],
        generation: 1,
    };
    let foreign = store
        .for_log(Some(other), Some(&lineage_of(&[(1, 9)])), &Steps::default())
        .unwrap();
    assert_eq!(foreign.cursor("attention"), None, "another id: a first run");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Where [`after_a_proved_remint`]'s bridge stores the step it proved, below the fork named.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum Stored {
    /// It stores none.
    Nowhere,
    /// In the notifier's own state dir, which the bridge shares.
    Shared(u64),
    /// In this Mac's fabric bridge's state dir, beside the notifier's ([`Config::bridge_state`]).
    Fabric(u64),
    /// As [`Stored::Fabric`], once a run on the new log failed while a directory stood in the
    /// steps file's place there, from before the first run on A: a steps read that fails fails
    /// only what the steps decide ([`Steps::get`]).
    Unread(u64),
}

/// A `--once` notifier (`--since since`) pages four escalations on a real broker's log A
/// (`help-0`..`help-3` at 0..=3, one presence subject); the broker stops, the log's identity
/// sidecar rots, and the log opens again under another id at generation 0 keeping every record
/// (astream's `logid::settle`, `Prior::Unreadable`): B. A fifth escalation, `help-9`, lands at
/// 4, and a bridge stores the step A→B it proved where `step` says ([`Stored`], as
/// `StateDir::add_proved_step` writes it, `proved-logs`); the notifier runs again, with the
/// carry's NEGATIVE CONTROL on where `forget` says so ([`FORGET_PROVED`]). Returns the pages of
/// that second run, `<offset> <text>` each, in order.
#[cfg(unix)]
fn after_a_proved_remint(tag: &str, since: Since, step: Stored, forget: bool) -> Vec<String> {
    let dir = scratch(tag);
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let log = base.join("r.log");
    let sock = base.join("r.sock");
    let fired = base.join("fired");
    let fabric = base.with_file_name("fabric");
    let mut c = cfg(&dir);
    c.bridge_state = Some(fabric.to_string_lossy().into_owned());
    c.broker = sock.to_string_lossy().into_owned();
    c.since = since;
    c.limit = 10;
    c.exec = format!(
        "printf '%s %s\\n' \"$ATERM_NOTIFY_OFFSET\" \"$ATERM_NOTIFY_TEXT\" >> {}",
        fired.display()
    );
    let escalate = |text: &str| {
        let (mut conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
        let seq = u64::from(text.as_bytes()[text.len() - 1] - b'0') + 1;
        let body = format!("v=1 t=1 attention={text}");
        conn.publish(
            7,
            seq,
            "/f/f1/pub/n-a/tx000000000/presence",
            body.as_bytes(),
        )
        .unwrap()
        .0
    };
    let run = || {
        Notifier::new(c.clone())
            .expect("open")
            .catch_up()
            .expect("catch up");
        std::fs::read_to_string(&fired).unwrap_or_default()
    };
    let unread = fabric.join("proved-logs");
    if matches!(step, Stored::Unread(_)) {
        std::fs::create_dir_all(&unread).unwrap();
    }
    let before = {
        let broker = crate::test_broker(&log).unwrap();
        let mut server = broker.serve(&sock).unwrap();
        assert_eq!(run(), "", "{tag}: an empty log: the cursor is claimed at 0");
        for text in ["help-0", "help-1", "help-2", "help-3"] {
            escalate(text);
        }
        assert_eq!(run().lines().count(), 4, "{tag}: all four fire on A");
        server.shutdown();
        broker.log_identity()
    };
    std::fs::write(astream_broker::identity_path(&log), b"not an identity").unwrap();
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let broker = loop {
        match crate::test_broker(&log) {
            Ok(broker) => break broker,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && Instant::now() < give_up => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{tag}: the log did not open again: {e}"),
        }
    };
    let now = broker.log_identity();
    assert!(
        now.id != before.id && now.generation == 0,
        "{tag}: PRECONDITION: the broker gave the log another id at generation 0: {before:?} -> \
         {now:?}"
    );
    let _ = std::fs::remove_file(&sock);
    let mut server = broker.serve(&sock).unwrap();
    assert_eq!(
        escalate("help-9"),
        4,
        "{tag}: PRECONDITION: B kept A's four records"
    );
    if matches!(step, Stored::Unread(_)) {
        let failed = Notifier::new(c.clone()).expect("open").catch_up();
        assert!(
            failed.is_err(),
            "{tag}: a run on a log no store is held for fails while the steps do not read"
        );
        assert!(
            !base.join("notify/logs").join(now.to_string()).exists(),
            "{tag}: and builds no store for it"
        );
        assert_eq!(
            std::fs::read_to_string(&fired).unwrap().lines().count(),
            4,
            "{tag}: and pages nothing"
        );
        std::fs::remove_dir(&unread).unwrap();
    }
    let stored = match step {
        Stored::Nowhere => None,
        Stored::Shared(fork) => Some((base.clone(), fork)),
        Stored::Fabric(fork) | Stored::Unread(fork) => Some((fabric, fork)),
    };
    if let Some((bridge, fork)) = stored {
        crate::state::StateDir::open(bridge)
            .unwrap()
            .add_proved_step(crate::state::Proved {
                was: before,
                now,
                fork,
                head: 4,
            })
            .unwrap();
    }
    FORGET_PROVED.with(|f| f.set(forget));
    let after = run().lines().skip(4).map(str::to_string).collect();
    FORGET_PROVED.with(|f| f.set(false));
    server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
    after
}

/// **A PROVED REMINT CARRIES THE NOTIFIER'S STORE BELOW ITS FORK** (ATERM-KI-1116-7314,
/// [`after_a_proved_remint`]): astream gave the log another id keeping every record, and a
/// bridge sharing the notifier's state dir, or this Mac's fabric bridge, proved and stored the
/// step. The notifier carries the store of the log before onto the new one as across a moved
/// generation, every cursor at most the fork and the verdicts below it: a step at its cursor
/// pages the new escalation alone, under `--since head` and `--since start` alike; a step below
/// it judges again the records from the fork (their verdicts dropped, the standing value kept,
/// which each of them changes), pages none below it and misses none. With no step stored the
/// new log is a first run: `--since head` never pages the escalation that landed before the run
/// (a page missed), `--since start` pages all five again. A steps file that does not read
/// ([`Stored::Unread`]) fails no run on A, whose store is the root's, and fails the run that
/// reached B with no store held for it, building none and paging nothing, so once it reads the
/// run carries as with the step there from the start (a read taken for no step would have built
/// B as a first run under `--since start`, all five paged again). NEGATIVE CONTROL
/// ([`FORGET_PROVED`], every build before): the step stored, the same first run.
#[cfg(unix)]
#[test]
fn a_proved_remint_carries_the_notifiers_store_below_its_fork() {
    for (since, step) in [
        (Since::Head, Stored::Shared(4)),
        (Since::Start, Stored::Shared(4)),
        (Since::Head, Stored::Fabric(4)),
        (Since::Start, Stored::Unread(4)),
    ] {
        let tag = format!("pr-{since:?}-{step:?}");
        assert_eq!(
            after_a_proved_remint(&tag, since, step, false),
            ["4 help-9"],
            "{tag}: carried at the fork, the cursor's own place"
        );
    }
    assert_eq!(
        after_a_proved_remint("pr-below", Since::Head, Stored::Shared(2), false),
        ["2 help-2", "3 help-3", "4 help-9"],
        "a fork below the cursor: the records from it are judged again"
    );
    for (tag, step, forget) in [
        ("pr-none", Stored::Nowhere, false),
        ("pr-ctl", Stored::Shared(4), true),
    ] {
        assert!(
            after_a_proved_remint(tag, Since::Head, step, forget).is_empty(),
            "{tag}: a first run starts past the escalation, a page missed"
        );
        assert_eq!(
            after_a_proved_remint(&format!("{tag}-0"), Since::Start, step, forget),
            ["0 help-0", "1 help-1", "2 help-2", "3 help-3", "4 help-9"],
            "{tag}: a first run from the start pages all five again"
        );
    }
}

/// The fabric root `aterm fabric on` makes at `root`, and the bridge command it writes there
/// ([`crate::enable::Paths::bridge_command`]), with that bridge's `--broker`.
fn fabric_on(root: &str) -> (crate::enable::Paths, Vec<String>, String) {
    let paths = crate::enable::Paths {
        home: PathBuf::from("/nonexistent/home"),
        login_home: None,
        root: PathBuf::from(root),
        aterm: "/nonexistent/bin/aterm".to_string(),
        fleet: "f1".to_string(),
        config: None,
        rendezvous: None,
        durability: None,
    };
    let argv = paths.bridge_command("n-0123456789abcdef");
    let broker = argv
        .iter()
        .skip_while(|w| *w != "--broker")
        .nth(1)
        .expect("the bridge names its broker")
        .clone();
    (paths, argv, broker)
}

/// The rendezvous file `aterm fabric on` writes for `paths` ([`crate::enable::Rendezvous`]).
fn rendezvous_of(
    paths: &crate::enable::Paths,
    argv: &[String],
    broker: &str,
) -> crate::enable::Rendezvous {
    crate::enable::Rendezvous {
        fleet: "f1".to_string(),
        broker: broker.to_string(),
        node: "n-0123456789abcdef".to_string(),
        cap_file: paths.cap().to_string_lossy().into_owned(),
        state: paths.state().to_string_lossy().into_owned(),
        command: argv.to_vec(),
    }
}

/// A real broker's log `base/r.log`, served on `base/r.sock` while `on_a` runs on it, then given
/// another id at generation 0 keeping every record (its identity sidecar rots: astream's
/// `logid::settle`, `Prior::Unreadable`, as in [`after_a_proved_remint`]) and served again: the
/// log before, the log now, the broker and its server.
#[cfg(unix)]
fn reminted_keeping_its_records(
    tag: &str,
    base: &Path,
    on_a: impl FnOnce(),
) -> (
    astream_broker::LogIdentity,
    astream_broker::LogIdentity,
    astream_broker::Broker,
    astream_broker::BrokerHandle,
) {
    let log = base.join("r.log");
    let sock = base.join("r.sock");
    let before = {
        let broker = crate::test_broker(&log).unwrap();
        let mut server = broker.serve(&sock).unwrap();
        on_a();
        server.shutdown();
        broker.log_identity()
    };
    std::fs::write(astream_broker::identity_path(&log), b"not an identity").unwrap();
    let (broker, server) = served_again(tag, base);
    let now = broker.log_identity();
    assert!(
        now.id != before.id && now.generation == 0,
        "{tag}: PRECONDITION: another id at generation 0: {before:?} -> {now:?}"
    );
    (before, now, broker, server)
}

/// The log `base/r.log` opened again once the broker that served it let it go, and served on
/// `base/r.sock`.
#[cfg(unix)]
fn served_again(tag: &str, base: &Path) -> (astream_broker::Broker, astream_broker::BrokerHandle) {
    let sock = base.join("r.sock");
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let broker = loop {
        match crate::test_broker(base.join("r.log")) {
            Ok(broker) => break broker,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && Instant::now() < give_up => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{tag}: the log did not open again: {e}"),
        }
    };
    let _ = std::fs::remove_file(&sock);
    let server = broker.serve(&sock).unwrap();
    (broker, server)
}

/// How [`after_a_late_step`]'s notifier reads the reminted log.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum Reads {
    /// A `--once` run at each step.
    Once,
    /// One follower ([`reader`], driven by [`pump`]) across them; before the step is stored,
    /// three of its polls of the steps by its clock alone, as [`Notifier::serve`] ticks it at
    /// [`Notifier::next_due`] when no notice arrives, each double the wait, and polls made at
    /// their own due take it to [`PROVED_POLL_MAX`], where it stays (its first wait 10 ms); once
    /// the step is stored, with its next poll brought due, by its clock alone and taking no
    /// notice: its own poll carries ([`Notifier::poll_proved`]).
    Clock,
    /// One follower whose poll is out of reach (its first wait an hour); once the step is
    /// stored, the broker's server restarts, and the reader's connect after it carries
    /// ([`Notice::Log`]).
    Reconnect,
}

/// A notifier (`--since head`) pages `help-0`..`help-3` (0..=3) on a real broker's log A, which
/// astream then gives another id at generation 0 keeping every record
/// ([`reminted_keeping_its_records`]): B, where `help-4` lands at 4 and is cleared at 5 before the
/// notifier reaches it. It reaches B BEFORE any bridge stored the step A→B: a first run,
/// starting at B's head, 6 ([`UNPROVED`]); `help-4` is raised again at 6 and pages there, so it
/// stands in B's own store. Only then does the bridge sharing its state dir store the step (fork
/// 4), and the notifier reads the steps again ([`Reads`]); a follower's reader then reads again
/// from the carried place ([`Hold::carried`]), and `help-6` lands at 7. With [`SKIP_RECARRY`]
/// where `skip` says so. Returns the pages after A's four, `<offset> <text>` each, in order, once
/// the selector's cursor passed every record.
#[cfg(unix)]
fn after_a_late_step(tag: &str, reads: Reads, skip: bool) -> Vec<String> {
    let dir = scratch(tag);
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let sock = base.join("r.sock");
    let fired = base.join("fired");
    let mut c = cfg(&dir);
    c.broker = sock.to_string_lossy().into_owned();
    c.since = Since::Head;
    c.limit = 10;
    c.exec = format!(
        "printf '%s %s\\n' \"$ATERM_NOTIFY_OFFSET\" \"$ATERM_NOTIFY_TEXT\" >> {}",
        fired.display()
    );
    let seq = std::cell::Cell::new(0);
    let publish = |body: &str| {
        let (mut conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
        seq.set(seq.get() + 1);
        conn.publish(
            7,
            seq.get(),
            "/f/f1/pub/n-a/tx000000000/presence",
            body.as_bytes(),
        )
        .unwrap()
        .0
    };
    let escalate = |text: &str| publish(&format!("v=1 t=1 attention={text}"));
    let pages = || -> Vec<String> {
        std::fs::read_to_string(&fired)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    };
    let run = || {
        Notifier::new(c.clone())
            .expect("open")
            .catch_up()
            .expect("catch up");
    };
    let (before, now, broker, server) = reminted_keeping_its_records(tag, &base, || {
        run();
        for text in ["help-0", "help-1", "help-2", "help-3"] {
            escalate(text);
        }
        run();
        assert_eq!(pages().len(), 4, "{tag}: all four fire on A");
    });
    assert_eq!(
        escalate("help-4"),
        4,
        "{tag}: PRECONDITION: B kept A's four"
    );
    assert_eq!(publish("v=1 t=1"), 5, "{tag}: help-4 cleared in the gap");
    let store_step = || {
        crate::state::StateDir::open(base.clone())
            .unwrap()
            .add_proved_step(crate::state::Proved {
                was: before,
                now,
                fork: 4,
                head: 6,
            })
            .unwrap();
    };
    let marker = base
        .join("notify/logs")
        .join(now.to_string())
        .join(UNPROVED);
    let key = Sel::Attention.key();
    let mut served = Some((broker, server));
    SKIP_RECARRY.with(|s| s.set(skip));
    match reads {
        Reads::Once => {
            run();
            assert!(marker.exists(), "{tag}: B began as a first run, marked");
            assert_eq!(escalate("help-4"), 6, "{tag}: help-4 raised again on B");
            run();
            store_step();
            run();
        }
        Reads::Clock | Reads::Reconnect => {
            let mut follow = c.clone();
            follow.once = false;
            let mut n = Notifier::new(follow.clone()).expect("open");
            n.proved_every = match reads {
                Reads::Clock => Duration::from_millis(10),
                _ => Duration::from_secs(3600),
            };
            let (tx, rx) = notice_handoff();
            let follower = {
                let hold = std::sync::Arc::clone(&n.hold);
                std::thread::spawn(move || reader(0, &Sel::Attention, &follow, &hold, &tx))
            };
            pump(&mut n, &rx, "the reader's subscription on B", |n| {
                n.log_now == Some(now)
                    && served
                        .as_ref()
                        .is_some_and(|(broker, _)| broker.stats().parked_tails == 1)
            });
            assert!(marker.exists(), "{tag}: B began as a first run, marked");
            assert_eq!(
                n.proved_poll.is_some(),
                !skip,
                "{tag}: the follower reads the steps again (CONTROL: owed nothing, it does not)"
            );
            assert_eq!(escalate("help-4"), 6, "{tag}: help-4 raised again on B");
            pump(&mut n, &rx, "help-4's page on B", |_| pages().len() == 5);
            if matches!(reads, Reads::Clock) && !skip {
                let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
                let wait_of = |n: &Notifier| {
                    n.proved_poll.unwrap_or_else(|| {
                        panic!("{tag}: no step stored: the marker stands and the follower polls")
                    })
                };
                let mut polled = wait_of(&n);
                let mut waits = vec![polled.1];
                while waits.len() < 4 {
                    assert!(
                        Instant::now() < give_up,
                        "{tag}: the follower's poll never came due"
                    );
                    let due = n.next_due().unwrap_or_else(|| {
                        panic!("{tag}: nothing is due by the follower's clock, its poll none")
                    });
                    std::thread::sleep(due.saturating_duration_since(Instant::now()));
                    let _ = n.tick(n.now());
                    let now = wait_of(&n);
                    if now.0 != polled.0 {
                        waits.push(now.1);
                    }
                    polled = now;
                }
                for _ in 0..16 {
                    if polled.1 == PROVED_POLL_MAX && waits.ends_with(&[PROVED_POLL_MAX; 2]) {
                        break;
                    }
                    n.poll_proved(polled.0);
                    polled = wait_of(&n);
                    waits.push(polled.1);
                }
                assert!(
                    waits
                        .windows(2)
                        .all(|w| w[1] == (w[0] * 2).min(PROVED_POLL_MAX))
                        && waits.ends_with(&[PROVED_POLL_MAX; 2]),
                    "{tag}: each read that carries nothing doubles the wait, three by the \
                     follower's clock and on by the poll's own due to {PROVED_POLL_MAX:?}, where \
                     it stays: {waits:?}"
                );
                n.proved_poll = Some((Instant::now(), polled.1));
            }
            store_step();
            match reads {
                Reads::Clock if !skip => {
                    let carried = n.hold.carried();
                    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
                    while n.hold.carried() == carried {
                        assert!(
                            Instant::now() < give_up,
                            "{tag}: the follower's poll never carried"
                        );
                        let due = n.next_due().unwrap_or_else(|| {
                            panic!("{tag}: nothing is due by the follower's clock, its poll none")
                        });
                        std::thread::sleep(due.saturating_duration_since(Instant::now()));
                        let _ = n.tick(n.now());
                    }
                }
                Reads::Reconnect => {
                    if let Some((broker, mut server)) = served.take() {
                        server.shutdown();
                        drop(server);
                        drop(broker);
                    }
                    let (broker, server) = served_again(tag, &base);
                    assert_eq!(broker.log_identity(), now, "{tag}: PRECONDITION: B again");
                    served = Some((broker, server));
                    if !skip {
                        assert_eq!(
                            pump(&mut n, &rx, "the carry at the reader's connect", |_| {
                                !marker.exists()
                            }),
                            "log",
                            "{tag}: the reader's connect carries, its poll out of reach"
                        );
                    }
                }
                _ => {}
            }
            assert_eq!(escalate("help-6"), 7);
            pump(&mut n, &rx, "every record judged", |n| {
                n.cursor(&key) == Some(8) && pages().iter().any(|p| p.starts_with("7 "))
            });
            drop(rx);
            escalate("help-7");
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the reader ends",
                || follower.is_finished().then_some(()),
            );
            follower.join().expect("the reader");
        }
    }
    SKIP_RECARRY.with(|s| s.set(false));
    let carried = !marker.exists();
    let after = pages().into_iter().skip(4).collect();
    if let Some((broker, mut server)) = served {
        server.shutdown();
        drop(broker);
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(carried, !skip, "{tag}: the marker goes with the carry");
    after
}

/// A `--once` notifier on `need` (`--since head`) reads a real broker's log A, where a session
/// opens and starts a shell command; astream then gives the log another id keeping every record
/// ([`reminted_keeping_its_records`]): B, where the command's close, a failure, lands at A's head
/// before the notifier reaches it, so the need rises in the gap, the session's first record past
/// A's cursor. The notifier reaches B BEFORE any bridge stored the step A→B: a first run at B's
/// head, whose warm-up finds the failure standing and seeds it in B's own store
/// ([`Notifier::seed_standing`]), so it pages nothing. Then the bridge sharing its state dir
/// stores the step (fork: A's head), and the next run carries A's store into B's. With
/// [`SKIP_RECARRY`] where `skip` says so. Returns the offsets paged, and the failure's.
#[cfg(unix)]
fn after_a_late_need(tag: &str, skip: bool) -> (Vec<u64>, u64) {
    let dir = scratch(tag);
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let sock = base.join("r.sock");
    let fired = base.join("fired");
    let mut c = cfg(&dir);
    c.broker = sock.to_string_lossy().into_owned();
    c.since = Since::Head;
    c.limit = 10;
    c.on = vec![Sel::Need(None)];
    c.exec = format!(
        "printf '%s\\n' \"$ATERM_NOTIFY_OFFSET\" >> {}",
        fired.display()
    );
    let seq = std::cell::Cell::new(0);
    let publish = |bodies: Vec<String>| -> Vec<(u64, String)> {
        let (mut conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
        bodies
            .into_iter()
            .map(|b| {
                seq.set(seq.get() + 1);
                let off = conn
                    .publish(7, seq.get(), NEED_FACE, b.as_bytes())
                    .unwrap()
                    .0;
                (off, b)
            })
            .collect()
    };
    let run = || {
        Notifier::new(c.clone())
            .expect("open")
            .catch_up()
            .expect("catch up");
    };
    let mut s = Needs::open();
    let cmd = s.cmd_open();
    let mut a_head = 0;
    let (before, now, broker, mut server) = reminted_keeping_its_records(tag, &base, || {
        a_head = publish(s.bodies()).last().map_or(0, |(off, _)| off + 1);
        run();
    });
    s.cmd_close(cmd, aterm_slog::RunOut::Fail);
    let gap = publish(s.bodies());
    let rose = gap
        .iter()
        .find(|(_, b)| b.contains(" op=close ") && b.contains("out=fail"))
        .map(|(off, _)| *off)
        .expect("the failed run's close");
    assert_eq!(
        (gap.first().map(|(off, _)| *off), rose),
        (Some(a_head), a_head),
        "{tag}: PRECONDITION: the failure is the session's first record past A's head"
    );
    let marker = base
        .join("notify/logs")
        .join(now.to_string())
        .join(UNPROVED);
    let offsets = || fired_offsets(&fired);
    SKIP_RECARRY.with(|s| s.set(skip));
    run();
    assert!(marker.exists(), "{tag}: B began as a first run, marked");
    assert!(
        offsets().is_empty(),
        "{tag}: its warm-up seeded the standing failure"
    );
    crate::state::StateDir::open(base.clone())
        .unwrap()
        .add_proved_step(crate::state::Proved {
            was: before,
            now,
            fork: a_head,
            head: a_head + 1,
        })
        .unwrap();
    run();
    SKIP_RECARRY.with(|s| s.set(false));
    let paged = offsets();
    server.shutdown();
    drop(broker);
    let _ = std::fs::remove_dir_all(&dir);
    (paged, rose)
}

/// **A STEP STORED AFTER THE NOTIFIER REACHED THE NEW LOG CARRIES ITS STORE STILL**
/// (ATERM-KI-1116-7314, the round's race: a reader usually reaches a reminted log before the
/// bridge's attach stores its step, [`after_a_late_step`]): the notifier began B as a first run
/// and paged `help-4` raised again there; once the step is stored, the next connect (`--once`),
/// the follower's own poll of the steps by its clock alone, or its reader's connect with the
/// poll out of reach, carries A's store below the fork into B's: `help-4`, raised and cleared
/// between A's cursor and B's first-run start, pages once, judged under A's standing values, not
/// under B's own (where the `help-4` B paged stands), and the `help-4` paged on B does not page
/// again. NEGATIVE CONTROL ([`SKIP_RECARRY`], the rule before): B stays a first run, and the
/// gap's `help-4` never pages.
#[cfg(unix)]
#[test]
fn a_step_stored_after_the_notifier_reached_the_new_log_carries_its_store_still() {
    for reads in [Reads::Once, Reads::Clock, Reads::Reconnect] {
        let tag = format!("late-{reads:?}");
        let mut want = vec!["6 help-4", "4 help-4"];
        if !matches!(reads, Reads::Once) {
            want.push("7 help-6");
        }
        assert_eq!(
            after_a_late_step(&tag, reads, false),
            want,
            "{tag}: the gap pages once, and nothing paged on B pages again"
        );
        want.retain(|p| !p.starts_with("4 "));
        assert_eq!(
            after_a_late_step(&format!("{tag}-ctl"), reads, true),
            want,
            "{tag}: CONTROL: a first run for good misses the gap's help-4"
        );
    }
}

/// **A NEED THAT ROSE BEFORE A LATE STEP PAGES ONCE THE STEP IS STORED** (ATERM-KI-1116-7314,
/// [`after_a_late_need`]): a failure that rose between A's cursor and B's first-run start, which
/// B's warm-up found standing and seeded in B's own store, pages once the step carries A's store
/// into B's, judged under A's standing values, not B's own. NEGATIVE CONTROL ([`SKIP_RECARRY`],
/// the rule before): B stays a first run, and it never pages.
#[cfg(unix)]
#[test]
fn a_need_that_rose_before_a_late_step_pages_once_the_step_is_stored() {
    let (paged, rose) = after_a_late_need("late-need", false);
    assert_eq!(paged, [rose], "a need that rose in the gap pages once");
    let (paged, _) = after_a_late_need("late-need-ctl", true);
    assert!(paged.is_empty(), "CONTROL: a first run for good misses it");
}

/// **A NOTIFIER READS THE STEPS OF THE FABRIC BRIDGE THAT DIALS ITS BROKER** ([`fabric_bridge_of`],
/// ATERM-KI-1116-7314): from the bridge command `aterm fabric on` writes (`aterm link serve …
/// --broker <root>/bus.sock --state <root>/link-state`), the bridge's `--state` where its
/// `--broker` is the notifier's; else from the rendezvous file's `broker` and `state`; none where
/// neither dials it, where the one that does is the notifier's own state dir (read already), or
/// where the words are no bridge's.
#[test]
fn a_notifier_reads_the_steps_of_the_fabric_bridge_that_dials_its_broker() {
    let (ours, argv, sock) = fabric_on("/r/aterm-fabric");
    let state = ours.state().to_string_lossy().into_owned();
    assert_eq!(
        state, "/r/aterm-fabric/link-state",
        "PRECONDITION: the layout `on` makes"
    );
    let (theirs, other, other_sock) = fabric_on("/o/aterm-fabric");
    let own = "/n/aterm-link";
    let ours_met = rendezvous_of(&ours, &argv, &sock);
    let theirs_met = rendezvous_of(&theirs, &other, &other_sock);
    let read = std::cell::Cell::new(0);
    for (tag, configured, rendezvous, state_dir, want) in [
        (
            "the configured",
            Some(argv.clone()),
            Some(theirs_met.clone()),
            own,
            Some(&state),
        ),
        (
            "the rendezvous's",
            None,
            Some(ours_met.clone()),
            own,
            Some(&state),
        ),
        (
            "the rendezvous's, the configured dials another",
            Some(other.clone()),
            Some(ours_met.clone()),
            own,
            Some(&state),
        ),
        (
            "none dials it",
            Some(other.clone()),
            Some(theirs_met.clone()),
            own,
            None,
        ),
        (
            "its own",
            Some(argv.clone()),
            Some(ours_met.clone()),
            state.as_str(),
            None,
        ),
        (
            "no bridge's words",
            Some(vec!["/bin/sleep".to_string(), "1".to_string()]),
            None,
            own,
            None,
        ),
    ] {
        assert_eq!(
            fabric_bridge_of(&sock, state_dir, configured, || {
                read.set(read.get() + 1);
                rendezvous
            }),
            want.cloned(),
            "{tag}"
        );
    }
    assert_eq!(
        read.get(),
        5,
        "the rendezvous is read only where the configured fails"
    );
}

const PARSE_PROBE: &str = "ATERM_NOTIFY_PARSE_PROBE";

/// [`parse_finds_the_fabric_bridge_aterm_launches`]'s child: `parse` of a notifier's flags on
/// the broker [`PARSE_PROBE`] names (before a tab) with `[fabric] command` set by its seam
/// (`ATERM_FABRIC_COMMAND`, in this child's environment alone), and the bridge state it must
/// find after the tab (`-` for none). Nothing where the probe is not set.
#[test]
fn parse_probe() {
    let Some(probe) = std::env::var_os(PARSE_PROBE) else {
        return;
    };
    let probe = probe.into_string().unwrap();
    let (broker, want) = probe.split_once('\t').unwrap();
    let words = [
        "--fleet",
        "f1",
        "--broker",
        broker,
        "--on",
        "attention",
        "--exec",
        "true",
        "--state",
        "/n/aterm-link",
    ];
    let args: Vec<String> = words.iter().map(|w| (*w).to_string()).collect();
    let cfg = parse(&args).expect("the flags parse");
    assert_eq!(
        cfg.bridge_state.as_deref(),
        (want != "-").then_some(want),
        "Config::bridge_state"
    );
    println!("\nnotify-parse-probe-complete");
}

/// **PARSE FINDS THE FABRIC BRIDGE ATERM LAUNCHES** (ATERM-KI-1116-7314, [`fabric_bridge_state`]):
/// with `[fabric] command` the bridge `aterm fabric on` writes, a notifier on that bridge's
/// `--broker` reads the steps of its `--state` (`Config::bridge_state`); on another broker, none.
/// Each runs in a child ([`parse_probe`]), so the seam's variable is in no other test's
/// environment, waited on while it makes progress (a loaded Mac stretches the wall, not it).
#[test]
fn parse_finds_the_fabric_bridge_aterm_launches() {
    let (ours, argv, sock) = fabric_on("/r/aterm-fabric");
    let state = ours.state().to_string_lossy().into_owned();
    let module = module_path!().split_once("::").unwrap().1;
    for (broker, want) in [(sock.as_str(), state.as_str()), ("/o/bus.sock", "-")] {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args(["--exact", &format!("{module}::parse_probe"), "--nocapture"])
            .env("ATERM_FABRIC_COMMAND", argv.join(" "))
            .env(PARSE_PROBE, format!("{broker}\t{want}"));
        let output = aterm_uds::deadline::output_while_process_progressing(
            &mut command,
            aterm_spec::wait::MUST_HAPPEN,
            "the notify parse probe",
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{broker}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == "notify-parse-probe-complete"),
            "{broker}: the probe ran: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

/// **A PROVED STEP CARRIES A HELD STORE AS A MOVED GENERATION DOES** (ATERM-KI-1116-7314): a
/// log of another id that a stored step says continues a held log ([`crate::state::Proved`]) is
/// built from that log's store, every cursor at most the step's fork, the verdicts below it and
/// the standing values as they are; a later generation of the step's log, which astream's
/// lineage says continues it, below the lower of the step's fork and that move's; a step from a
/// log not held, or one to another log, carries nothing; and where a held store of the new log's
/// own id continues by the lineage too, that store, taken up after the step, is the one carried.
/// NEGATIVE CONTROL ([`FORGET_PROVED`], the rule before): each a first run, the lineage's
/// carry as it was.
#[test]
fn a_proved_step_carries_the_store_it_continues_below_its_fork() {
    use crate::state::Proved;
    let log = |id: u8, generation| astream_broker::LogIdentity {
        id: [id; 16],
        generation,
    };
    let step = |was, now, fork| Proved {
        was,
        now,
        fork,
        head: 7,
    };
    let presence = "/f/f1/pub/n-a/tx000000000/presence";
    for control in [false, true] {
        let dir = scratch(if control { "proved-ctl" } else { "proved" });
        let store = Store::open(&dir).expect("open");
        let a = log(7, 0);
        let root = store.for_log(Some(a), None, &Steps::default()).unwrap();
        root.set_cursor("attention", 9).unwrap();
        root.set_journal(
            &["2 fired rc=0".to_string(), "6 fired rc=0".to_string()],
            Flush::Ordered,
        )
        .unwrap();
        root.set_standing(&[(presence.into(), "help".into())])
            .unwrap();
        FORGET_PROVED.with(|f| f.set(control));
        let carried = |s: &Store| (s.cursor("attention"), s.journal(), s.standing());
        let first_run = (None, Vec::new(), Vec::new());
        let below = |fork| {
            (
                Some(fork),
                vec!["2 fired rc=0".to_string()],
                vec![(presence.to_string(), "help".to_string())],
            )
        };

        let b = store
            .for_log(Some(log(8, 0)), None, &[step(a, log(8, 0), 5)].into())
            .unwrap();
        assert_eq!(
            carried(&b),
            if control { first_run.clone() } else { below(5) },
            "control={control}: the step's log, at its fork"
        );
        let lineage = lineage_of(&[(1, 4), (2, 6)]);
        let c = store
            .for_log(
                Some(log(9, 2)),
                Some(&lineage),
                &[step(a, log(9, 0), 5)].into(),
            )
            .unwrap();
        assert_eq!(
            carried(&c),
            if control { first_run.clone() } else { below(4) },
            "control={control}: a later generation of it, at the lower fork"
        );
        for (tag, now, steps) in [
            (
                "from a log not held",
                log(10, 0),
                [step(log(1, 0), log(10, 0), 5)],
            ),
            ("to another log", log(11, 0), [step(a, log(12, 0), 5)]),
        ] {
            let other = store.for_log(Some(now), None, &steps.into()).unwrap();
            assert_eq!(
                carried(&other),
                first_run,
                "control={control}: a step {tag}"
            );
        }

        b.set_cursor("attention", 8).unwrap();
        let newer = store
            .for_log(
                Some(log(8, 1)),
                Some(&lineage_of(&[(1, 6)])),
                &[step(a, log(8, 0), 5)].into(),
            )
            .unwrap();
        assert_eq!(
            newer.cursor("attention"),
            Some(6),
            "control={control}: the store of the new log's own id, by its lineage"
        );
        FORGET_PROVED.with(|f| f.set(false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// **A STORE BEGUN AS A FIRST RUN WAITS FOR A STEP AND IS CARRIED INTO ONCE ONE REACHES IT**
/// (ATERM-KI-1116-7314): a log no held store continues is built as a first run with an
/// [`UNPROVED`] marker naming the logs held then, in one publish (a write killed before it
/// leaves no store and no scratch); [`Store::unproved`] waits while no step reaches it, and once
/// one does from a log the marker names, [`Store::recarry`] carries that log's store below the
/// fork into it: each cursor the lower of its own and the held one at most the fork (a key only
/// its own has as it is, one only the held store has, here above the fork, at the fork), the
/// held verdicts below the fork joined with its own (none it paged pages again), the held
/// store's standing values, never its own; carried again from the same store while its marker
/// stands, the same journal. A marker whose words name no log is read as naming the root's. A
/// store built from a marked one, by its lineage or by a step from it, is not marked
/// ([`Store::carry`]): a first run for good, owed nothing though a step from the root's log
/// reaches it through the lineage (the residual: the records below the marked one's first-run
/// start are never judged there), while the marked one is owed the root's store still. A store
/// whose own journal holds [`JOURNAL_KEEP`] verdicts is
/// [`Unproved::Spent`]: no carry. A store whose own journal is damaged before an intact step,
/// the load's rewrite refused so the damage stays on the media, is read as the load salvaged it
/// ([`Store::load`]) and carried into; one whose held store's journal does not read, or one of
/// whose held stores does not open, is carried nothing and waits, its marker kept
/// ([`Unproved::unread`]). NEGATIVE CONTROL
/// ([`SKIP_RECARRY`], the rule before): never owed a carry, a first run for good.
#[test]
fn a_store_begun_as_a_first_run_waits_for_a_step_and_is_carried_into_once_one_reaches_it() {
    use crate::state::Proved;
    let log = |id: u8, generation| astream_broker::LogIdentity {
        id: [id; 16],
        generation,
    };
    let step = |was, now, fork| Proved {
        was,
        now,
        fork,
        head: 9,
    };
    let presence = "/f/f1/pub/n-a/tx000000000/presence";
    let own_value = ("/f/f1/pub/n-a/ty000000000/presence", "disk");
    let (a, b) = (log(7, 0), log(8, 0));
    for control in [false, true] {
        let dir = scratch(if control { "unproved-ctl" } else { "unproved" });
        let store = Store::open(&dir).expect("open");
        let root = store.for_log(Some(a), None, &Steps::default()).unwrap();
        root.set_cursor("attention", 9).unwrap();
        root.set_cursor("ask-to.h-a", 7).unwrap();
        root.set_journal(
            &["2 fired rc=0".to_string(), "6 fired rc=0".to_string()],
            Flush::Ordered,
        )
        .unwrap();
        root.set_standing(&[(presence.into(), "help".into())])
            .unwrap();
        let logs = PathBuf::from(&dir).join("notify/logs");
        let marker = |of: astream_broker::LogIdentity| logs.join(of.to_string()).join(UNPROVED);

        PUT_FAULT.with(|left| left.set(Some(0)));
        let killed = store.for_log(Some(b), None, &Steps::default());
        PUT_FAULT.with(|left| left.set(None));
        assert!(killed.is_err(), "the marker's write is killed");
        assert!(
            !logs.join(b.to_string()).exists(),
            "no store is published without its marker"
        );
        let scratch_left = std::fs::read_dir(&logs)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().starts_with('.'));
        assert!(!scratch_left, "no scratch left behind");

        let first = store.for_log(Some(b), None, &Steps::default()).unwrap();
        assert_eq!(
            std::fs::read_to_string(marker(b)).unwrap().trim(),
            a.to_string(),
            "a first run beside A's store names A"
        );
        assert_eq!(first.cursor("attention"), None, "a first run");
        first.set_cursor("attention", 12).unwrap();
        first.set_cursor("halt", 20).unwrap();
        first
            .set_journal(&["13 fired rc=0".to_string()], Flush::Ordered)
            .unwrap();
        first
            .set_standing(&[(own_value.0.into(), own_value.1.into())])
            .unwrap();
        SKIP_RECARRY.with(|s| s.set(control));
        let owed = |of: &Store,
                    id: astream_broker::LogIdentity,
                    lineage: Option<&astream_broker::Lineage>,
                    proved: &[Proved]| {
            match store
                .unproved(
                    id,
                    lineage,
                    &proved.to_vec().into(),
                    &of.read_verdicts().unwrap(),
                )
                .unwrap()
            {
                None => "none",
                Some(Unproved::Wait) => "wait",
                Some(Unproved::Spent) => "spent",
                Some(Unproved::Carry(..)) => "carry",
            }
        };
        let carry = |into: &Store,
                     id: astream_broker::LogIdentity,
                     lineage: Option<&astream_broker::Lineage>,
                     proved: &[Proved]| {
            let mut file = into.load().unwrap();
            store
                .recarry(&mut file, id, lineage, &proved.to_vec().into())
                .unwrap()
        };
        let waits = if control { "none" } else { "wait" };
        assert_eq!(owed(&first, b, None, &[]), waits, "no step yet");
        assert_eq!(
            owed(&first, b, None, &[step(log(1, 0), b, 5)]),
            waits,
            "a step from a log not held"
        );
        assert_eq!(
            owed(&first, b, None, &[step(a, log(9, 0), 5)]),
            waits,
            "a step to another log"
        );
        let proved = [step(a, b, 5)];
        match carry(&first, b, None, &proved) {
            Some(Unproved::Carry(from, fork)) => {
                assert!(!control, "CONTROL: never owed a carry");
                assert_eq!((from.root.clone(), fork), (store.root.clone(), 5));
            }
            None => assert!(control, "a step from A reaches B: carry"),
            other => panic!("control={control}: {other:?}"),
        }
        let read = |key: &str| first.cursor(key);
        if control {
            assert_eq!(
                (read("attention"), read("ask-to.h-a"), first.journal()),
                (Some(12), None, vec!["13 fired rc=0".to_string()]),
                "CONTROL: B as its first run left it"
            );
        } else {
            assert_eq!(
                (read("attention"), read("halt"), read("ask-to.h-a")),
                (Some(5), Some(20), Some(5)),
                "the lower of its own and A's at most the fork; one only B has as it is; one only \
                 A has, above the fork, at the fork"
            );
            assert_eq!(
                first.journal(),
                ["2 fired rc=0", "13 fired rc=0"],
                "A's verdicts below the fork, then B's own"
            );
            assert_eq!(
                first.standing(),
                [(presence.to_string(), "help".to_string())],
                "A's standing values, never B's own"
            );
            let once = first.read_verdicts().unwrap();
            assert!(
                matches!(carry(&first, b, None, &proved), Some(Unproved::Carry(..))),
                "the marker stands: owed again"
            );
            assert_eq!(
                first.read_verdicts().unwrap(),
                once,
                "carried again from the same store: the same journal"
            );
            first.drop_unproved().unwrap();
            assert!(!marker(b).exists(), "the marker is removed");
            assert_eq!(owed(&first, b, None, &proved), "none", "carried once");
        }

        let c = log(10, 0);
        let damaged = store.for_log(Some(c), None, &Steps::default()).unwrap();
        std::fs::write(marker(c), "not a log\n").unwrap();
        assert_eq!(
            owed(&damaged, c, None, &[step(a, c, 5)]),
            if control { "none" } else { "carry" },
            "control={control}: a marker naming no log reads as naming the root's"
        );

        let d = log(11, 0);
        let marked = store.for_log(Some(d), None, &Steps::default()).unwrap();
        marked.set_cursor("attention", 30).unwrap();
        let moved = lineage_of(&[(1, 25)]);
        let d1 = store
            .for_log(Some(log(11, 1)), Some(&moved), &Steps::default())
            .unwrap();
        assert_eq!(
            d1.cursor("attention"),
            Some(25),
            "control={control}: carried by its lineage from D's first run"
        );
        let e = log(12, 0);
        let by_step = store
            .for_log(Some(e), None, &[step(log(11, 1), e, 20)].into())
            .unwrap();
        assert!(
            by_step.cursor("attention").is_some(),
            "control={control}: carried by a step from D's next generation, not begun as a first \
             run"
        );
        for (of, why) in [(log(11, 1), "by its lineage"), (e, "by a step")] {
            assert!(
                !marker(of).exists(),
                "control={control}: a store built {why} from a marked one is not marked, a first \
                 run for good"
            );
        }
        let a_to_d = [step(a, d, 5)];
        assert_eq!(
            owed(&marked, d, None, &a_to_d),
            if control { "none" } else { "carry" },
            "control={control}: D itself, still marked, is owed A's store by a step from A"
        );
        assert_eq!(
            owed(&d1, log(11, 1), Some(&moved), &a_to_d),
            "none",
            "control={control}: D's next generation, though the step reaches it by the lineage, \
             is owed nothing: the records below D's first-run start are never judged there"
        );
        assert!(
            carry(&d1, log(11, 1), Some(&moved), &a_to_d).is_none(),
            "control={control}: nothing carried into it"
        );
        assert_eq!(
            d1.cursor("attention"),
            Some(25),
            "control={control}: D's next generation keeps the place it was carried to"
        );

        let f = log(13, 0);
        let full = store.for_log(Some(f), None, &Steps::default()).unwrap();
        for (lines, want) in [(JOURNAL_KEEP - 1, "carry"), (JOURNAL_KEEP, "spent")] {
            let verdicts: Vec<String> = (0..lines)
                .map(|i| format!("{} fired rc=0", 100 + i))
                .collect();
            full.set_journal(&verdicts, Flush::Ordered).unwrap();
            assert_eq!(
                owed(&full, f, None, &[step(a, f, 5)]),
                if control { "none" } else { want },
                "control={control}: its own journal holding {lines} verdicts"
            );
        }

        let g = log(14, 0);
        let faulted = store.for_log(Some(g), None, &Steps::default()).unwrap();
        let verdicts: Vec<journal::Step> = (20..=22)
            .map(|off| journal::Step {
                verdict: Some(format!("{off} fired rc=0")),
                ..journal::Step::default()
            })
            .collect();
        let (mut damaged, _) = journal::encode_from(0, &verdicts).unwrap();
        let (first_step, _) = journal::encode_from(0, &verdicts[..1]).unwrap();
        damaged[first_step.len() + 20] ^= 0x55;
        std::fs::write(faulted.root.join(JOURNAL_FILE), &damaged).unwrap();
        assert!(
            faulted.read_verdicts().is_err(),
            "PRECONDITION: damage before an intact step, which a strict read refuses"
        );
        REFUSE.with(|r| *r.borrow_mut() = vec![(JOURNAL_FILE, 1, libc::ENOSPC)]);
        let owed_faulted = carry(&faulted, g, None, &[step(a, g, 5)]);
        assert!(
            REFUSE.with(|r| r.borrow().is_empty()),
            "PRECONDITION: the load's rewrite refused, the damage left on the media"
        );
        assert_eq!(
            matches!(owed_faulted, Some(Unproved::Carry(..))),
            !control,
            "control={control}: its own journal read as the load salvaged it"
        );
        if !control {
            assert_eq!(faulted.cursor("attention"), Some(5), "carried at the fork");
        }

        let h = log(15, 0);
        let waiting = store.for_log(Some(h), None, &Steps::default()).unwrap();
        std::fs::write(store.root.join(JOURNAL_FILE), &damaged).unwrap();
        assert!(
            store.read_verdicts().is_err(),
            "PRECONDITION: the root's journal, the store a step from A carries, does not read"
        );
        let owed_unread = carry(&waiting, h, None, &[step(a, h, 5)]);
        assert_eq!(
            matches!(owed_unread, Some(Unproved::Wait)),
            !control,
            "control={control}: a held store that does not read carries nothing, waiting: \
             {owed_unread:?}"
        );
        assert!(
            marker(h).exists() && waiting.cursor("attention").is_none(),
            "control={control}: its marker kept, the first run as it began"
        );
        std::fs::write(logs.join(log(16, 0).to_string()), b"not a store").unwrap();
        assert_eq!(
            owed(&waiting, h, None, &[step(a, h, 5)]),
            if control { "none" } else { "wait" },
            "control={control}: a held store that does not open: waiting, nothing decided"
        );
        SKIP_RECARRY.with(|s| s.set(false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// **A CARRY IS COUNTED ONCE ITS JOURNAL IS WRITTEN, AND A MARKER WHOSE REMOVAL IS REFUSED IS
/// REMOVED AGAIN** (ATERM-KI-1116-7314, [`Notifier::drop_marker`]): a notifier takes up B,
/// begun as a first run, once a step from the root's A reaches it, and the marker's removal is
/// refused: the take-up stands (the refusal is said, not propagated), the carry is on the media
/// and counted ([`Hold::carried`]), and the follower's poll removes the marker, carrying
/// nothing again. A next run (`--once`) that finds the marker standing once records were judged
/// past the fork (as the run before's catch-up judges them: its cursor at 40, a verdict at 30, a
/// standing value they set) carries the same store into the merged journal again: the cursor
/// falls to the fork again, every verdict kept and A's standing values back, so what was judged
/// since is judged again and pages nothing again. NEGATIVE CONTROLS (forced once by hand): the
/// removal's error propagated fails the take-up; the carry counted after the removal, or the
/// removal not owed again, leaves the marker standing for good; the claim before, that the
/// merge made again leaves the journal as the first left it, fails on the next run's.
#[test]
fn a_carry_is_counted_once_written_and_a_marker_whose_removal_is_refused_is_removed_again() {
    let log = |id: u8| astream_broker::LogIdentity {
        id: [id; 16],
        generation: 0,
    };
    let (a, b) = (log(7), log(8));
    let presence = "/f/f1/pub/n-a/tx000000000/presence";
    let standing_of_a = vec![(presence.to_string(), "help".to_string())];
    let dir = scratch("drop-refused");
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir).expect("open");
    let root = store.for_log(Some(a), None, &Steps::default()).unwrap();
    root.set_cursor("attention", 9).unwrap();
    root.set_journal(&["2 fired rc=0".to_string()], Flush::Ordered)
        .unwrap();
    root.set_standing(&standing_of_a).unwrap();
    let first = store.for_log(Some(b), None, &Steps::default()).unwrap();
    first.set_cursor("attention", 12).unwrap();
    first
        .set_journal(&["13 fired rc=0".to_string()], Flush::Ordered)
        .unwrap();
    drop((root, first, store));
    let marker = PathBuf::from(&dir)
        .join("notify/logs")
        .join(b.to_string())
        .join(UNPROVED);
    let read = ReadLog {
        id: Some(b),
        lineage: None,
        proved: [crate::state::Proved {
            was: a,
            now: b,
            fork: 5,
            head: 9,
        }]
        .into(),
        ordered: false,
        carried: 0,
    };
    let mut lines = vec!["2 fired rc=0", "13 fired rc=0"];
    for run in ["follower", "next run"] {
        let (mut n, _fired) = appending(&dir, 3, "");
        REFUSE.with(|r| *r.borrow_mut() = vec![(UNPROVED, 0, libc::EACCES)]);
        n.scope_to(&read)
            .unwrap_or_else(|e| panic!("{run}: the take-up stands, the refusal said: {e}"));
        assert!(
            REFUSE.with(|r| r.borrow().is_empty()),
            "{run}: PRECONDITION: the removal was refused"
        );
        assert_eq!(n.hold.carried(), 1, "{run}: the carry is counted");
        assert!(marker.exists(), "{run}: the marker stands");
        let journal = n.log_store.read_verdicts().unwrap();
        assert_eq!(
            (
                journal.cursors.get("attention"),
                journal.lines.iter().map(String::as_str).collect::<Vec<_>>(),
                &journal.standing
            ),
            (Some(&5), lines.clone(), &standing_of_a),
            "{run}: the carry is on the media: the cursor at the fork, every verdict kept, A's \
             standing values (merged again, what was judged since falls below the cursor again, \
             its verdicts kept, so it pages nothing again)"
        );
        if run == "follower" {
            drop(n);
            let store = Store::open(&dir).expect("open");
            let judged = store.for_log(Some(b), None, &Steps::default()).unwrap();
            lines.push("30 fired rc=0");
            judged
                .set_journal(
                    &lines.iter().map(|l| (*l).to_string()).collect::<Vec<_>>(),
                    Flush::Ordered,
                )
                .unwrap();
            judged.set_cursor("attention", 40).unwrap();
            judged
                .set_standing(&[(presence.to_string(), "disk".to_string())])
                .unwrap();
            assert_eq!(
                judged.cursor("attention"),
                Some(40),
                "PRECONDITION: run N judged past the fork with the marker standing"
            );
            continue;
        }
        let (due, _) = n
            .proved_poll
            .unwrap_or_else(|| panic!("{run}: the removal is owed again"));
        n.poll_proved(due);
        assert!(!marker.exists(), "{run}: the follower's poll removed it");
        assert_eq!(
            (n.hold.carried(), n.proved_poll),
            (1, None),
            "{run}: carrying nothing again, its poll ends"
        );
        assert_eq!(
            n.log_store.read_verdicts().unwrap(),
            journal,
            "{run}: its journal as the carry left it"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A STEP CARRIES THE STORE THIS NOTIFIER USED LAST, AT THE HIGHEST FORK ITS STEPS PROVE**
/// (ATERM-KI-1116-7314, [`Store::carried_from`]): where steps from two held logs both reach the
/// new log, the store taken up last (`logs/used`, the root's take-up noted there too) is
/// carried, whatever the steps' order (a rank by a step's place in the joined list would carry
/// the other in one of the two orders: the control); every step from a held log is a proof of
/// its own, so one that reaches the new log carries its store though a later step from that log
/// went to another; and of a store's steps that reach it, the highest fork, in either order.
#[test]
fn a_step_carries_the_store_this_notifier_used_last_at_the_highest_fork_its_steps_prove() {
    use crate::state::Proved;
    let log = |id: u8| astream_broker::LogIdentity {
        id: [id; 16],
        generation: 0,
    };
    let step = |was, now, fork| Proved {
        was,
        now,
        fork,
        head: 50,
    };
    let dir = scratch("proved-rank");
    let store = Store::open(&dir).expect("open");
    let (a, c, d) = (log(7), log(8), log(9));
    store
        .for_log(Some(a), None, &Steps::default())
        .unwrap()
        .set_cursor("attention", 30)
        .unwrap();
    for (held, at) in [(c, 10), (d, 20)] {
        store
            .for_log(Some(held), None, &Steps::default())
            .unwrap()
            .set_cursor("attention", at)
            .unwrap();
    }
    let carried = |x: u8, steps: &[Proved]| {
        store
            .for_log(Some(log(x)), None, &steps.to_vec().into())
            .unwrap()
            .cursor("attention")
    };
    let take_up = |logs: &[astream_broker::LogIdentity]| {
        for held in logs {
            store.for_log(Some(*held), None, &Steps::default()).unwrap();
        }
    };
    let mut x = 20;
    for (first, last, want) in [(d, c, 10), (c, d, 20)] {
        for order in [[c, d], [d, c]] {
            take_up(&[first, last]);
            x += 1;
            let steps = [step(order[0], log(x), 40), step(order[1], log(x), 40)];
            assert_eq!(
                carried(x, &steps),
                Some(want),
                "{:?} taken up last, steps from {:?} first: its store",
                last.id[0],
                order[0].id[0]
            );
        }
    }
    take_up(&[c, d, a]);
    x += 1;
    assert_eq!(
        carried(x, &[step(a, log(x), 40), step(d, log(x), 40)]),
        Some(30),
        "the root's store, taken up again last"
    );
    take_up(&[c]);
    x += 1;
    assert_eq!(
        carried(x, &[step(c, log(x), 4), step(c, log(60), 50)]),
        Some(4),
        "a step from C that reaches it carries C's store, though C's newest went elsewhere"
    );
    for forks in [[4, 8], [8, 4]] {
        take_up(&[c]);
        x += 1;
        assert_eq!(
            carried(x, &[step(c, log(x), forks[0]), step(c, log(x), forks[1])]),
            Some(8),
            "of C's steps that reach it, the highest fork, the steps in the order {forks:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A STEPS FILE THAT DOES NOT READ IS AN ERROR, NOT NO STEP** ([`proved_steps`], [`Steps`]): one
/// missing is none, one there reads as its steps, and one there that does not read (here a
/// directory in its place, in the notifier's state dir or the fabric bridge's) fails the read;
/// and that fails only the build of a store for a log not the root's ([`Store::for_log`]: none
/// is built), never the root's own log nor a store that stands, so a connect to either goes on;
/// a store still marked [`UNPROVED`] is taken up waiting, its marker kept, carried nothing by a
/// read that may have missed a step though one is stored, and the next read that reads decides
/// ([`Store::unproved`]). NEGATIVE CONTROL: the lenient read (`StateDir::proved_steps`) takes it
/// for none.
#[test]
fn a_steps_file_that_does_not_read_is_an_error_not_no_step() {
    let dir = scratch("proved-read");
    let base = PathBuf::from(&dir);
    let fabric = base.join("fabric");
    std::fs::create_dir_all(&fabric).unwrap();
    let mut c = cfg(&dir);
    c.bridge_state = Some(fabric.to_string_lossy().into_owned());
    std::fs::create_dir_all(&base).unwrap();
    assert!(proved_steps(&c).unwrap().is_empty(), "none stored");
    let log = |id: u8| astream_broker::LogIdentity {
        id: [id; 16],
        generation: 0,
    };
    let (a, b) = (log(7), log(8));
    std::fs::write(fabric.join("proved-logs"), format!("{a}~{b}@4\n")).unwrap();
    assert_eq!(proved_steps(&c).unwrap().len(), 1, "the fabric bridge's");
    for unread in [base.join("proved-logs"), fabric.join("proved-logs")] {
        let _ = std::fs::remove_file(&unread);
        std::fs::create_dir_all(&unread).unwrap();
        assert!(
            proved_steps(&c).is_err(),
            "{}: one there that does not read",
            unread.display()
        );
        assert!(
            crate::state::StateDir::peek(unread.parent().unwrap())
                .proved_steps()
                .is_empty(),
            "CONTROL: the lenient read takes it for none"
        );
        std::fs::remove_dir(&unread).unwrap();
    }

    let store = Store::open(&dir).expect("open");
    let unread = fabric.join("proved-logs");
    let _ = std::fs::remove_file(&unread);
    std::fs::create_dir_all(&unread).unwrap();
    let failed = Steps::read(&c);
    assert!(failed.0.is_err(), "PRECONDITION: the read fails");
    store
        .for_log(Some(a), None, &failed)
        .expect("the first log read: the root's, no step decides");
    store
        .for_log(Some(a), None, &failed)
        .expect("the root's own log again");
    assert!(
        store.for_log(Some(b), None, &failed).is_err(),
        "a log not the root's, no store held for it"
    );
    assert!(
        !base.join("notify/logs").join(b.to_string()).exists(),
        "none is built"
    );
    let a1 = astream_broker::LogIdentity {
        id: [7; 16],
        generation: 1,
    };
    store
        .for_log(Some(a1), Some(&lineage_of(&[(1, 3)])), &failed)
        .expect("a moved generation of a held log: its lineage decides, no step");
    assert!(
        base.join("notify/logs").join(a1.to_string()).exists()
            && !base
                .join("notify/logs")
                .join(a1.to_string())
                .join(UNPROVED)
                .exists(),
        "carried by its lineage, not begun as a first run"
    );
    let marked = store.for_log(Some(b), None, &Steps::default()).unwrap();
    store
        .for_log(Some(b), None, &failed)
        .expect("a store that stands is returned as it is");
    let own = marked.read_verdicts().unwrap();
    std::fs::write(base.join("proved-logs"), format!("{a}~{b}@4\n")).unwrap();
    assert!(
        Steps::read(&c).0.is_err(),
        "PRECONDITION: the fabric bridge's file still does not read"
    );
    assert!(
        matches!(
            store.unproved(b, None, &failed, &own),
            Ok(Some(Unproved::Wait))
        ),
        "a marker that stands: taken up waiting, no carry decided by a read that failed"
    );
    assert!(
        base.join("notify/logs")
            .join(b.to_string())
            .join(UNPROVED)
            .exists(),
        "and its marker kept"
    );
    std::fs::remove_dir(&unread).unwrap();
    assert!(
        matches!(
            store.unproved(b, None, &Steps::read(&c), &own),
            Ok(Some(Unproved::Carry(_, 4)))
        ),
        "the next read that reads decides: the step stored carries"
    );
    marked.drop_unproved().unwrap();
    assert!(
        store.unproved(b, None, &failed, &own).unwrap().is_none(),
        "no marker: nothing decided"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A MARKED STORE WHOSE STEPS DO NOT READ IS TAKEN UP WAITING, AND CARRIED ONCE THEY READ**
/// (ATERM-KI-1116-7314, [`Store::unproved`], [`Unproved::unread`]): a follower whose reader
/// reached B, begun as a first run marked [`UNPROVED`] beside A's store, while the steps file
/// does not read (a directory in its place; the reader's connect-time read failed too), takes B
/// up as the first run it began as at its first record and pages it and the next; once the file
/// reads and holds the step A→B, its own poll reads the steps afresh
/// ([`Notifier::poll_proved`]), not the reader's failed read, and carries A's store below the
/// fork into B's, every verdict kept. NEGATIVE CONTROL (forced once by hand): the take-up failing
/// on the steps read, as before, stalls the record unpaged, and every retry reads the same
/// failed steps.
#[test]
fn a_marked_store_whose_steps_do_not_read_is_taken_up_waiting_and_carried_once_they_read() {
    let log = |id: u8| astream_broker::LogIdentity {
        id: [id; 16],
        generation: 0,
    };
    let (a, b) = (log(7), log(8));
    let presence = |sid: &str| format!("/f/f1/pub/n-a/{sid}/presence");
    let raise = |text: &str| format!("v=1 t=1 attention={text}").into_bytes();
    let dir = scratch("unread-marked");
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).unwrap();
    let store = Store::open(&dir).expect("open");
    let root = store.for_log(Some(a), None, &Steps::default()).unwrap();
    root.set_cursor("attention", 9).unwrap();
    let marked = store.for_log(Some(b), None, &Steps::default()).unwrap();
    drop((root, marked, store));
    let marker = base.join("notify/logs").join(b.to_string()).join(UNPROVED);
    assert!(
        marker.exists(),
        "PRECONDITION: B began as a first run, marked"
    );
    let steps = base.join("proved-logs");
    std::fs::create_dir_all(&steps).unwrap();
    let (mut n, fired) = appending(&dir, 10, "");
    let read = ReadLog {
        id: Some(b),
        lineage: None,
        proved: Steps::read(&n.cfg),
        ordered: false,
        carried: 0,
    };
    assert!(
        read.proved.0.is_err(),
        "PRECONDITION: the reader's read of the steps failed"
    );
    n.take(Notice::Log(0, read)).expect("its log");
    for (off, sid, token) in [(13, "ta000000000", "help"), (15, "tb000000000", "disk")] {
        let taken = n.take(Notice::Record(0, off, presence(sid), raise(token)));
        assert!(
            taken.is_ok(),
            "the record at {off} is taken while the steps do not read: {taken:?}"
        );
    }
    assert_eq!(
        (n.log_now, fired_offsets(&fired)),
        (Some(b), vec![13, 15]),
        "B taken up as its first run, and its records paged"
    );
    assert!(marker.exists(), "its marker kept");
    let (due, _) = n
        .proved_poll
        .unwrap_or_else(|| panic!("the follower reads the steps again"));
    n.poll_proved(due);
    assert_eq!(
        (n.hold.carried(), marker.exists()),
        (0, true),
        "a poll while the file does not read carries nothing and keeps the marker"
    );
    std::fs::remove_dir(&steps).unwrap();
    crate::state::StateDir::open(base.clone())
        .unwrap()
        .add_proved_step(crate::state::Proved {
            was: a,
            now: b,
            fork: 5,
            head: 16,
        })
        .unwrap();
    let (due, _) = n
        .proved_poll
        .unwrap_or_else(|| panic!("the follower reads the steps again"));
    n.poll_proved(due);
    assert_eq!(
        (n.hold.carried(), marker.exists(), n.proved_poll),
        (1, false, None),
        "once the file reads, the poll carries and removes the marker"
    );
    let held = n.log_store.read_verdicts().unwrap();
    assert_eq!(
        (
            held.cursors.get("attention"),
            held.lines
                .iter()
                .filter_map(|line| journal::offset_of(line))
                .collect::<Vec<_>>()
        ),
        (Some(&5), vec![13, 15]),
        "A's store carried below the fork, B's verdicts kept"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A READER'S START AT THE HEAD IS ON THE MEDIA BEFORE IT READS A RECORD: the notifier writes
/// a [`Notice::Start`] as the selector's cursor, ordered, and answers once it is written, so a
/// reader that reads on only after that answer has a restart re-read from that head, never
/// from a later one; a refused write is answered with its error (the reader connects again
/// after its backoff) and owes nothing. The NEGATIVE CONTROL is the start handed over as a
/// checkpoint: under the same refusal it is owed, not written, and nothing tells the reader,
/// which would read on past a start a kill then loses.
#[test]
fn a_readers_start_at_the_head_is_written_before_it_reads() {
    let key = Sel::Attention.key();
    let dir = scratch("start");
    let (mut n, _) = appending(&dir, 100, "");
    n.take(Notice::Log(0, strict_log())).expect("log");
    let on_media = |n: &Notifier| n.log_store.read_only().read_cursor(&key);
    let start = |n: &mut Notifier, head| {
        let (done, wrote) = mpsc::sync_channel(1);
        n.take(Notice::Start(0, head, done)).expect("take");
        wrote.try_recv().expect("answered at once")
    };
    REFUSE.with(|r| *r.borrow_mut() = vec![("cur/attention", 0, libc::ENOSPC)]);
    assert_eq!(
        start(&mut n, 9).map_err(|e| e.raw_os_error()),
        Err(Some(libc::ENOSPC)),
        "a refused start is answered with its error"
    );
    assert_eq!(
        (on_media(&n), n.lag.contains_key(&key)),
        (None, false),
        "and owes nothing"
    );
    start(&mut n, 9).expect("the start written");
    assert_eq!(on_media(&n), Some(9), "on the media before the answer");

    let dir = scratch("start-ctl");
    let (mut n, _) = appending(&dir, 100, "");
    n.take(Notice::Log(0, strict_log())).expect("log");
    REFUSE.with(|r| *r.borrow_mut() = vec![("cur/attention", 0, libc::ENOSPC)]);
    n.take(Notice::Checkpoint(0, 9)).expect("taken");
    assert_eq!(
        (on_media(&n), n.lag.get(&key).map(|lag| lag.want)),
        (None, Some(9)),
        "NEGATIVE CONTROL: a start handed over as a checkpoint is only owed, and the reader \
         is not told"
    );
    REFUSE.with(std::cell::RefCell::take);
}

#[test]
fn the_executor_holds_a_record_until_a_durable_mark_covers_it() {
    let presence = |sid: &str| format!("/f/f1/pub/n-a/{sid}/presence");
    let raise = |text: &str| format!("v=1 t=1 attention={text}").into_bytes();
    let log = |ordered: bool, generation: u64| ReadLog {
        id: Some(astream_broker::LogIdentity {
            id: [7; 16],
            generation,
        }),
        lineage: None,
        proved: Steps::default(),
        ordered,
        carried: 0,
    };
    let feed = |n: &mut Notifier, notices: Vec<Notice>| {
        for notice in notices {
            n.take(notice).expect("take");
        }
    };
    let cursor = |n: &Notifier| n.log_store.cursor(&Sel::Attention.key());

    let dir = scratch("held");
    let (mut n, fired) = appending(&dir, 100, "");
    feed(
        &mut n,
        vec![
            Notice::Log(0, log(true, 0)),
            Notice::Record(0, 5, presence("tzzzzzzzzza"), raise("key")),
            Notice::Record(0, 7, presence("tzzzzzzzzzb"), raise("disk")),
            Notice::Checkpoint(0, 9),
        ],
    );
    assert!(fired_offsets(&fired).is_empty(), "held, not paged");
    assert_eq!(cursor(&n), None, "and nothing moves past them");
    n.take(Notice::Durable(0, 5)).unwrap();
    assert!(
        fired_offsets(&fired).is_empty(),
        "5 is not below a durable 5"
    );
    n.take(Notice::Durable(0, 6)).unwrap();
    assert_eq!(fired_offsets(&fired), [5]);
    assert_eq!(cursor(&n), Some(6), "past the paged one, at the held one");
    n.take(Notice::Durable(0, 8)).unwrap();
    assert_eq!(fired_offsets(&fired), [5, 7]);
    assert_eq!(cursor(&n), Some(9), "the held-back checkpoint");
    assert_eq!(n.held_bytes, 0);

    feed(
        &mut n,
        vec![
            Notice::Record(0, 10, presence("tzzzzzzzzzc"), raise("lost")),
            Notice::Log(0, log(true, 1)),
            Notice::Durable(0, 99),
        ],
    );
    assert_eq!(
        fired_offsets(&fired),
        [5, 7],
        "another log's offset pages nothing"
    );
    assert_eq!(n.held_bytes, 0);

    feed(
        &mut n,
        vec![
            Notice::Log(0, log(true, 1)),
            Notice::Record(0, 20, presence("tzzzzzzzzzf"), raise("held")),
            Notice::Log(0, log(false, 1)),
            Notice::Record(0, 21, presence("tzzzzzzzzzg"), raise("strict")),
        ],
    );
    assert_eq!(fired_offsets(&fired), [5, 7, 20, 21], "in read order");
    assert_eq!(n.held_bytes, 0);
    n.take(Notice::Log(0, log(true, 1))).unwrap();

    feed(
        &mut n,
        vec![
            Notice::Record(0, 30, presence("tzzzzzzzzzd"), raise("first")),
            Notice::Record(0, 31, presence("tzzzzzzzzze"), {
                let mut body = raise("big");
                body.resize(HOLD_MAX, b' ');
                body
            }),
        ],
    );
    assert_eq!(fired_offsets(&fired), [5, 7, 20, 21, 30, 31]);
    assert_eq!(n.held_bytes, 0);
    let _ = std::fs::remove_dir_all(&dir);

    for (tag, ordered, wait) in [("strict", false, true), ("early", true, false)] {
        let dir = scratch(tag);
        let (mut n, fired) = appending(&dir, 100, "");
        n.wait_durable = wait;
        feed(
            &mut n,
            vec![
                Notice::Log(0, log(ordered, 0)),
                Notice::Record(0, 5, presence("tzzzzzzzzza"), raise("key")),
                Notice::Checkpoint(0, 9),
            ],
        );
        assert_eq!(fired_offsets(&fired), [5], "{tag}: paged as read");
        assert_eq!(cursor(&n), Some(9), "{tag}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A FOLLOWER SAYS A STANDING CONDITION ONCE (fix unit E5, E2E-15): on an ordered log,
/// fifty records each held and then released by a durable mark say the durable wait once,
/// a reconnect to the same log says nothing new, and another log says it once more; three
/// records past [`HOLD_MAX`] in one streak say the hold once, a durable head that reaches
/// the streak's newest paged record but not past it keeps the streak, one past it ends it
/// (the next hold says it once more), and so does another log, whose drain emptied the
/// queue. NEGATIVE CONTROLS (scratch): the wait said whenever the held queue is empty, as
/// before, says it fifty times; a streak no durable mark ends says the second hold never;
/// a streak another log does not end says the third never.
#[test]
fn a_durable_wait_is_said_once_per_log_and_a_hold_past_its_cap_once_per_streak() {
    let log = |generation: u64| ReadLog {
        id: Some(astream_broker::LogIdentity {
            id: [7; 16],
            generation,
        }),
        lineage: None,
        proved: Steps::default(),
        ordered: true,
        carried: 0,
    };
    let live = || b"v=1 t=1 state=live".to_vec();
    let said = |what: fn(&Warned) -> bool| {
        WARNED.with(|warned| warned.borrow().iter().filter(|w| what(w)).count())
    };
    let waits = || said(|w| matches!(w, Warned::DurableWait(0)));
    let dir = scratch("said-once");
    let (mut n, fired) = appending(&dir, 100, "");
    assert!(
        n.wait_durable,
        "PRECONDITION: an ordered log's records wait"
    );
    WARNED.with(|warned| warned.borrow_mut().clear());
    n.take(Notice::Log(0, log(0))).expect("log");
    for off in 0..50 {
        n.take(Notice::Record(0, off, presence_of("tzzzzzzzzza"), live()))
            .expect("held");
        assert_eq!(
            n.held_bytes,
            presence_of("tzzzzzzzzza").len() + live().len()
        );
        n.take(Notice::Durable(0, off + 1)).expect("released");
        assert_eq!(n.held_bytes, 0, "{off}: released");
    }
    assert_eq!(waits(), 1, "fifty held records say the wait once");
    n.take(Notice::Log(0, log(0))).expect("the same log again");
    n.take(Notice::Record(0, 50, presence_of("tzzzzzzzzza"), live()))
        .expect("held");
    n.take(Notice::Durable(0, 51)).expect("released");
    assert_eq!(waits(), 1, "a reconnect to the same log says nothing new");
    n.take(Notice::Log(0, log(1))).expect("another log");
    n.take(Notice::Record(0, 60, presence_of("tzzzzzzzzza"), live()))
        .expect("held");
    n.take(Notice::Durable(0, 61)).expect("released");
    assert_eq!(waits(), 2, "another log's wait is said once more");
    for off in 70..73 {
        let mut big = live();
        big.resize(HOLD_MAX, b' ');
        n.take(Notice::Record(0, off, presence_of("tzzzzzzzzza"), big))
            .expect("paged past the cap");
        assert_eq!(n.held_bytes, 0, "{off}: released at once");
    }
    let holds = || said(|w| matches!(w, Warned::HoldOver(0)));
    assert_eq!(holds(), 1, "one streak");
    let past_cap = |n: &mut Notifier, off: u64| {
        let mut big = live();
        big.resize(HOLD_MAX, b' ');
        n.take(Notice::Record(0, off, presence_of("tzzzzzzzzza"), big))
            .expect("paged past the cap");
        assert_eq!(n.held_bytes, 0, "{off}: released at once");
    };
    n.take(Notice::Durable(0, 72)).expect("durable below 72");
    past_cap(&mut n, 80);
    assert_eq!(holds(), 1, "72 is not durable yet: the streak goes on");
    n.take(Notice::Durable(0, 81)).expect("durable past 80");
    past_cap(&mut n, 90);
    assert_eq!(holds(), 2, "the broker said the streak durable: a new one");
    n.take(Notice::Log(0, log(2))).expect("another log");
    past_cap(&mut n, 100);
    assert_eq!(holds(), 3, "another log's hold is said once more");
    assert!(
        fired_offsets(&fired).is_empty(),
        "PRECONDITION: no row paged"
    );
    let help = usage();
    let ceiling = format!("back-off up to {} s", transport::RECONNECT_MAX.as_secs());
    assert!(
        help.contains(&ceiling) && !help.contains("{RECONNECT_MAX}"),
        "the help's retry ceiling is the constant: {help}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A log a follow-mode reader reached that serves strict: a record is judged as it is read.
fn strict_log() -> ReadLog {
    ReadLog {
        id: Some(astream_broker::LogIdentity {
            id: [7; 16],
            generation: 0,
        }),
        lineage: None,
        proved: Steps::default(),
        ordered: false,
        carried: 0,
    }
}

fn presence_of(sid: &str) -> String {
    format!("/f/f1/pub/n-a/{sid}/presence")
}

/// By command, the flushes asked for since the one before ([`flushes_by_command`]).
type ByCommand = std::rc::Rc<std::cell::RefCell<Vec<Vec<Flush>>>>;

/// The flushes this thread asked for before [`Notifier::exec`] started each command
/// ([`AT_EXEC`]), and the recording that goes on after.
fn flushes_by_command() -> (ByCommand, std::rc::Rc<aterm_durable::seam::Recording>) {
    let recording = std::rc::Rc::new(aterm_durable::seam::Recording::start());
    let before = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = (std::rc::Rc::clone(&recording), std::rc::Rc::clone(&before));
    AT_EXEC.with(|seam| {
        *seam.borrow_mut() = Some(Box::new(move |_: &Hit| {
            seen.1.borrow_mut().push(seen.0.take());
        }));
    });
    (before, recording)
}

#[test]
fn a_page_flushes_in_full_once_and_before_its_command_runs() {
    use Flush::{Durable as D, Ordered as O};
    let durable = |flushes: &[Flush]| flushes.iter().filter(|f| f.is_durable()).count();
    let row = |off: u64, sid: &str, attention: &str| {
        Notice::Record(
            0,
            off,
            presence_of(sid),
            format!("v=1 t=1 state=live attention={attention}").into_bytes(),
        )
    };
    /// A case's name, its `firing` flush, what it judges before the recording starts, the
    /// pages it records, and what each page asks for before its command: the general law
    /// is one full flush, last. The journal's one step (its `firing` line and the stamp it
    /// spends) is that flush, where the log's store is the root's; in a log's own store the
    /// line's step is ordered ahead of the stamp's, whose flush it is.
    type Case = (
        &'static str,
        Flush,
        Vec<Notice>,
        Vec<Notice>,
        &'static [Flush],
    );
    let cases: [Case; 4] = [
        (
            "a first raise",
            D,
            Vec::new(),
            vec![
                row(1, "tzzzzzzzzza", "help-a"),
                row(2, "tzzzzzzzzzb", "help-b"),
            ],
            &[D],
        ),
        (
            "NEGATIVE CONTROL: an ordered `firing` line",
            O,
            Vec::new(),
            vec![
                row(1, "tzzzzzzzzza", "help-a"),
                row(2, "tzzzzzzzzzb", "help-b"),
            ],
            &[O],
        ),
        (
            "a raise that replaces another token, its cursor owed",
            D,
            vec![row(1, "tzzzzzzzzza", "help-1"), row(2, "tzzzzzzzzzq", "-")],
            vec![row(3, "tzzzzzzzzza", "help-2")],
            &[O, D],
        ),
        (
            "a log's own store, under logs/",
            D,
            Vec::new(),
            vec![
                row(1, "tzzzzzzzzza", "help-a"),
                row(2, "tzzzzzzzzzb", "help-b"),
            ],
            &[O, D],
        ),
    ];
    for (at, (case, firing, setup, pages, asks)) in cases.into_iter().enumerate() {
        let dir = scratch(&format!("page-flush-{at}"));
        if case.contains("logs/") {
            Store::open(&dir)
                .expect("open")
                .for_log(
                    Some(astream_broker::LogIdentity {
                        id: [8; 16],
                        generation: 0,
                    }),
                    None,
                    &Steps::default(),
                )
                .expect("the root's store claimed by another log");
        }
        let (mut n, fired) = appending(&dir, 100, "");
        n.firing = firing;
        n.cursor_every = Duration::MAX;
        n.take(Notice::Log(0, strict_log())).expect("log");
        n.take(Notice::Checkpoint(0, 1))
            .expect("the log's store claimed, its cursor written, before the recording");
        for notice in setup {
            n.take(notice).expect("judged before the recording");
        }
        let paged = fired_offsets(&fired).len();
        let (before, recording) = flushes_by_command();
        let mut after = Vec::new();
        let count = pages.len();
        for notice in pages {
            n.take(notice).expect("a page");
            after.push(recording.take());
        }
        AT_EXEC.with(|seam| seam.borrow_mut().take());
        let before = before.take();
        assert_eq!(
            fired_offsets(&fired).len() - paged,
            count,
            "{case}: each page ran its command"
        );
        assert_eq!(before.len(), count, "{case}: once");
        for (page, (before, after)) in before.iter().zip(&after).enumerate() {
            assert_eq!(
                before.as_slice(),
                asks,
                "{case}, page {page}: what it asks for before its command"
            );
            if firing == O {
                assert_eq!(
                    durable(before),
                    0,
                    "{case}: nothing is on the media before the command runs"
                );
                continue;
            }
            assert_eq!(
                (durable(before), before.last()),
                (1, Some(&D)),
                "{case}, page {page}: one full flush before its command, the last"
            );
            assert!(
                !after.is_empty() && durable(after) == 0,
                "{case}, page {page}: its `fired` line, standing value and cursor ordered \
                 after its command, none in full: {after:?}"
            );
            assert_eq!(
                after.first(),
                Some(&O),
                "{case}, page {page}: its `fired` line one ordered append: {after:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_record_that_pages_nothing_flushes_nothing_and_the_cursor_trails_it_by_at_most_its_lag() {
    let mut s = Needs::open();
    let start = s.bodies();
    for _ in 0..2 * CURSOR_LAG + 3 {
        s.quiet();
    }
    let mut records: Vec<(String, Vec<u8>)> = Vec::new();
    for (i, body) in s.bodies().into_iter().enumerate() {
        records.push((NEED_FACE.to_string(), body.into_bytes()));
        if i % 3 == 2 {
            records.push((
                presence_of("tzzzzzzzzzx"),
                b"v=1 t=1 inc=1 state=live".to_vec(),
            ));
        }
    }
    let need = Sel::Need(None);
    let key = need.key();
    for (lag, every) in [
        (CURSOR_LAG, Duration::MAX),
        (1, Duration::MAX),
        (CURSOR_LAG, Duration::ZERO),
    ] {
        let tag = format!("lag {lag}, every {every:?}");
        let dir = scratch(&format!("lag-{lag}-{}", every.is_zero()));
        let (mut n, fired) = appending(&dir, 100, "");
        n.cfg.on = vec![need.clone()];
        n.cursor_lag = lag;
        n.cursor_every = every;
        n.take(Notice::Log(0, strict_log())).expect("log");
        let mut off = 0;
        for body in &start {
            off += 1;
            n.take(Notice::Record(
                0,
                off,
                NEED_FACE.into(),
                body.clone().into_bytes(),
            ))
            .expect("the session's start");
        }
        n.persist().expect("written before the recording");
        let recording = aterm_durable::seam::Recording::start();
        let mut flushed = Vec::new();
        for (judged, (subject, body)) in (1..).zip(&records) {
            off += 1;
            n.take(Notice::Record(0, off, subject.clone(), body.clone()))
                .expect("a record");
            if !recording.take().is_empty() {
                flushed.push(judged);
            }
            let at = n.log_store.cursor(&key).expect("a cursor");
            assert!(
                off + 1 - at <= lag,
                "{tag}: the cursor at {at} trails @{off} by more than {lag}"
            );
        }
        assert!(fired_offsets(&fired).is_empty(), "{tag}: nothing rose");
        let all: Vec<u64> = (1..=records.len() as u64).collect();
        if lag == 1 {
            assert_eq!(
                flushed, all,
                "NEGATIVE CONTROL: a cursor written per record flushes per record"
            );
        } else if every.is_zero() {
            assert_eq!(
                flushed, all,
                "a cursor owed for its whole interval is written by the next record"
            );
        } else {
            let each_lag: Vec<u64> = all
                .iter()
                .copied()
                .filter(|j| j % CURSOR_LAG == 0)
                .collect();
            assert!(each_lag.len() >= 2, "PRECONDITION: two lags of records");
            assert_eq!(
                flushed, each_lag,
                "a record that pages nothing flushes nothing; its cursor is written once \
                 every {CURSOR_LAG}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_standing_value_never_reaches_the_media_ahead_of_its_selectors_cursor() {
    let key = Sel::Attention.key();
    let raise = b"v=1 t=1 attention=help".to_vec();
    let dir = scratch("standing-cursor");
    let (mut n, fired) = appending(&dir, 100, "");
    n.cursor_every = Duration::MAX;
    n.take(Notice::Log(0, strict_log())).expect("log");
    let records = [
        (1, "tzzzzzzzzza", raise.clone()),
        (2, "tzzzzzzzzza", raise.clone()),
        (3, "tzzzzzzzzza", b"v=1 t=1 attention=-".to_vec()),
        (4, "tzzzzzzzzzb", b"v=1 t=1 state=live".to_vec()),
    ];
    for (off, sid, body) in &records {
        n.take(Notice::Record(0, *off, presence_of(sid), body.clone()))
            .expect("a record");
    }
    assert_eq!(fired_offsets(&fired), [1], "the republish at 2 stood");
    assert_eq!(
        n.log_store.cursor(&key),
        Some(3),
        "the clear at 3 wrote the cursor AT 3 before it wrote the standing value"
    );
    drop(n);

    let replay = |cursor: u64| {
        let (mut back, _) = appending(&dir, 100, "");
        back.take(Notice::Log(0, strict_log())).expect("log");
        for (off, sid, body) in records.iter().filter(|(off, ..)| *off >= cursor) {
            back.take(Notice::Record(0, *off, presence_of(sid), body.clone()))
                .expect("a record re-read");
        }
        fired_offsets(&fired)
    };
    assert_eq!(
        replay(3),
        [1],
        "a crash that loses the owed cursor re-reads from 3, and pages nothing again"
    );
    Store::open(&dir)
        .expect("retired notifier store")
        .put_cursor_at(&key, 2)
        .expect("CONTROL: the cursor as it stood before the clear");
    assert_eq!(
        replay(2),
        [1, 2],
        "NEGATIVE CONTROL: the clear on the media ahead of the cursor pages the republish \
         it judged under the standing value"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE NOTIFIER'S STANDING CHANGE AND ITS CURSOR REACH THE MEDIA TOGETHER: the bytes
/// [`Notifier::put_standing`] appends for a clear (record 3 clears the value record 1 raised),
/// cut at every byte, read with the clear only where the selector's cursor is at the clearing
/// record or past it: the production write, not a hand-built step. The NEGATIVE CONTROL writes
/// the change and its cursor as two steps in one append (`Notifier::standing_split`), with the
/// same flushes, and a cut between them reads the clear with the cursor behind it.
#[test]
fn the_notifiers_standing_change_and_its_cursor_reach_the_media_together() {
    let key = Sel::Attention.key();
    let raise = b"v=1 t=1 attention=help".to_vec();
    for mutant in [false, true] {
        let dir = scratch(&format!("standing-cut-{mutant}"));
        let (mut n, _) = appending(&dir, 100, "");
        n.cursor_every = Duration::MAX;
        n.standing_split = mutant;
        n.take(Notice::Log(0, strict_log())).expect("log");
        for (off, body) in [(1, raise.clone()), (2, raise.clone())] {
            n.take(Notice::Record(0, off, presence_of("tzzzzzzzzza"), body))
                .expect("a record");
        }
        let path = n.log_store.root.join(journal::FILE);
        let before = std::fs::metadata(&path).expect("the journal").len();
        n.take(Notice::Record(
            0,
            3,
            presence_of("tzzzzzzzzza"),
            b"v=1 t=1 attention=-".to_vec(),
        ))
        .expect("the clear");
        let bytes = std::fs::read(&path).expect("the journal");
        let (mut apart, mut cleared_seen) = (0, false);
        for cut in usize::try_from(before).expect("small")..=bytes.len() {
            let held = journal::read(&bytes[..cut]).index;
            let cleared = !held
                .standing
                .iter()
                .any(|(s, _)| *s == presence_of("tzzzzzzzzza"));
            cleared_seen |= cleared;
            apart += usize::from(cleared && held.cursors.get(&key).is_none_or(|&c| c < 3));
        }
        assert!(
            cleared_seen,
            "PRECONDITION ({mutant}): the clear reached the journal"
        );
        if mutant {
            assert!(
                apart > 0,
                "NEGATIVE CONTROL: two steps in one append leave a cut with the clear ahead of \
                 its cursor"
            );
        } else {
            assert_eq!(
                apart, 0,
                "no cut reads the clear with its selector's cursor behind it"
            );
        }
    }
}

/// The cursor writes ([`PUT_NAMES`]) a `need` follower makes over a fleet's busy faces: one
/// at each page and each need it clears, one a second by its clock, and one per
/// [`CURSOR_LAG`] records at most, never one a record.
#[test]
fn a_followers_cursor_is_written_per_page_and_per_second_never_per_record() {
    let need = Sel::Need(None);
    let mut s = Needs::open();
    let mut bodies = s.bodies();
    for _ in 0..3 {
        for _ in 0..4 {
            for _ in 0..50 {
                s.quiet();
            }
            bodies.extend(s.bodies());
        }
        let asked = s.question();
        bodies.extend(s.bodies());
        s.answer(asked);
        bodies.extend(s.bodies());
    }
    let mut records: Vec<(String, Vec<u8>)> = Vec::new();
    for (i, body) in bodies.into_iter().enumerate() {
        records.push((NEED_FACE.to_string(), body.into_bytes()));
        if i % 2 == 1 {
            records.push((
                presence_of("tzzzzzzzzzy"),
                b"v=1 t=1 inc=1 state=live".to_vec(),
            ));
        }
    }
    let n_records = u32::try_from(records.len()).expect("a small run");
    let step = Duration::from_millis(50);
    let dir = scratch("cursor-writes");
    let (mut n, fired) = appending(&dir, 100, "");
    n.cfg.on = vec![need.clone()];
    let base = Instant::now();
    n.clock = Some(base);
    n.take(Notice::Log(0, strict_log())).expect("log");
    PUT_NAMES.with(|names| *names.borrow_mut() = Some(Vec::new()));
    for ((at, off), (subject, body)) in (0..).zip(1..).zip(&records) {
        n.clock = Some(base + step * at);
        n.take(Notice::Record(0, off, subject.clone(), body.clone()))
            .expect("a record");
    }
    let names = PUT_NAMES
        .with(|names| names.borrow_mut().take())
        .unwrap_or_default();
    let cursors = names.iter().filter(|name| name.starts_with("cur/")).count();
    let pages = fired_offsets(&fired).len();
    assert_eq!(pages, 3, "PRECONDITION: each question paged once");
    let seconds = usize::try_from((step * n_records).as_secs()).expect("seconds");
    let lags = records.len() / usize::try_from(CURSOR_LAG).expect("lag");
    let bound = 3 * pages + seconds + lags + 2;
    assert!(
        records.len() > 4 * bound,
        "PRECONDITION: a bound well short of a write per record"
    );
    assert!(
        cursors <= bound,
        "{} records, {pages} pages over {seconds} s: {cursors} cursor writes, past \
         {bound} (two a page and one a clear, one a second, one a lag)",
        records.len()
    );
    n.clock = Some(base + step * n_records + CURSOR_EVERY);
    n.tick(n.now()).expect("a tick");
    assert_eq!(
        n.log_store.cursor(&need.key()),
        Some(u64::from(n_records) + 1),
        "a second after the last record, its cursor is written"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Records as a test feeds them: offset, subject, body.
type Records = Vec<(u64, String, Vec<u8>)>;

/// A session's start, then three failed runs, each lifted by the good run after it ([`Needs`]),
/// at offsets from 1; and the offsets of the failed runs' closes, each a rise to `failure`.
fn three_failures() -> (Records, Vec<u64>) {
    let mut s = Needs::open();
    let mut bodies = s.bodies();
    for _ in 0..3 {
        let bad = s.cmd_open();
        s.cmd_close(bad, aterm_slog::RunOut::Fail);
        bodies.extend(s.bodies());
        let good = s.cmd_open();
        s.cmd_close(good, aterm_slog::RunOut::Ok);
        bodies.extend(s.bodies());
    }
    let rises = (1..)
        .zip(&bodies)
        .filter(|(_, b)| b.contains(" op=close ") && b.contains("out=fail"))
        .map(|(off, _)| off)
        .collect();
    let records = (1..)
        .zip(bodies)
        .map(|(off, b)| (off, NEED_FACE.to_string(), b.into_bytes()))
        .collect();
    (records, rises)
}

#[test]
fn a_refused_write_never_ends_the_follower_and_each_rise_pages_once() {
    let (records, rises) = three_failures();
    assert_eq!(rises.len(), 3, "PRECONDITION: three rises");
    let need = Sel::Need(None);
    let dir = scratch("refused");
    let (mut n, fired) = appending(&dir, 100, "");
    n.cfg.on = vec![need.clone()];
    n.cursor_every = Duration::MAX;
    n.retry_every = Duration::ZERO;
    WARNED.with(|warned| warned.borrow_mut().clear());
    let enospc = libc::ENOSPC;
    REFUSE.with(|refuse| {
        *refuse.borrow_mut() = vec![
            ("cur/", 1, enospc),
            ("standing", 0, enospc),
            ("journal", 2, enospc),
            ("journal", 1, enospc),
        ];
    });
    let mut notices = vec![Notice::Log(0, strict_log())];
    notices.extend(
        records
            .iter()
            .map(|(off, subject, body)| Notice::Record(0, *off, subject.clone(), body.clone())),
    );
    let (tx, rx) = mpsc::sync_channel(notices.len());
    for notice in notices {
        tx.send(notice).expect("queued");
    }
    drop(tx);
    let ended = n.serve(&rx);
    assert_eq!(
        ended.map_err(|e| e.kind()),
        Err(io::ErrorKind::ConnectionAborted),
        "the follower took every notice and ended only with its readers"
    );
    assert_eq!(
        fired_offsets(&fired),
        rises,
        "each rise paged once: the one whose `firing` line was refused once it was written"
    );
    let unspent = REFUSE.with(std::cell::RefCell::take);
    assert!(
        unspent.is_empty(),
        "PRECONDITION: the first page's cursor and standing value, the second's `firing` \
         and `fired` lines were refused: {unspent:?} left"
    );
    let warned = WARNED.with(std::cell::RefCell::take);
    assert!(
        warned.contains(&Warned::Refused(Some(enospc))),
        "a refusal is warned of with its errno: {warned:?}"
    );
    assert_eq!(
        (warned.last(), n.failing.is_none()),
        (Some(&Warned::Cleared), true),
        "and so is the end of it: {warned:?}"
    );
    assert_eq!(
        n.log_store.cursor(&need.key()),
        Some(records.len() as u64 + 1),
        "every cursor owed is written"
    );
    let journal = n.log_store.journal();
    assert!(
        rises.iter().all(|off| journal
            .iter()
            .any(|l| l.starts_with(&format!("{off} fired ")) && l.contains(" rc=0 "))),
        "each page's `fired` line is on the store: {journal:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_page_whose_firing_line_is_refused_runs_its_command_only_once_it_is_written() {
    let (records, rises) = three_failures();
    let need = Sel::Need(None);
    let open = |dir: &str| {
        let (mut n, fired) = appending(dir, 100, "");
        n.cfg.on = vec![need.clone()];
        n.cursor_every = Duration::MAX;
        n.retry_every = Duration::from_secs(60);
        n.take(Notice::Log(0, strict_log())).expect("log");
        (n, fired)
    };
    let record = |off: u64| {
        let (off, subject, body) = &records[usize::try_from(off - 1).expect("an index")];
        Notice::Record(0, *off, subject.clone(), body.clone())
    };
    for crash in [false, true] {
        let dir = scratch(&format!("firing-refused-{crash}"));
        let (mut n, fired) = open(&dir);
        let base = Instant::now();
        n.clock = Some(base);
        REFUSE.with(|refuse| *refuse.borrow_mut() = vec![("journal", 0, libc::ENOSPC)]);
        for off in 1..rises[0] {
            n.take(record(off)).expect("judged");
        }
        let stalled = n.take(record(rises[0]));
        assert!(
            fired_offsets(&fired).is_empty(),
            "crash={crash}: its command did not run without its `firing` line"
        );
        assert!(
            REFUSE.with(std::cell::RefCell::take).is_empty(),
            "PRECONDITION: the page's `firing` line was refused"
        );
        assert_eq!(
            stalled.map_err(|e| e.raw_os_error()),
            Err(Some(libc::ENOSPC)),
            "the page stalls on the refusal"
        );
        let waits = n.take(record(rises[0] + 1));
        assert_eq!(
            (waits.map_err(|e| e.kind()), fired_offsets(&fired).len()),
            (Err(io::ErrorKind::WouldBlock), 0),
            "crash={crash}: a notice taken before the retry is due waits behind the page"
        );
        let (mut n, fired) = if crash {
            drop(n);
            let (mut back, fired) = open(&dir);
            let from = back.log_store.cursor(&need.key()).unwrap_or(1);
            assert!(from <= rises[0], "PRECONDITION: the cursor is not past it");
            for off in from..=rises[0] + 1 {
                back.take(record(off)).expect("re-read");
            }
            (back, fired)
        } else {
            n.clock = Some(base + Duration::from_secs(60));
            n.tick(n.now()).expect("the retry runs it");
            (n, fired)
        };
        assert_eq!(
            fired_offsets(&fired),
            [rises[0]],
            "crash={crash}: the page ran once, once its `firing` line was written"
        );
        for off in rises[0] + 2..=records.len() as u64 {
            n.take(record(off)).expect("judged");
        }
        assert_eq!(
            fired_offsets(&fired),
            rises,
            "crash={crash}: each rise once"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_crash_after_a_page_whose_cursor_was_refused_runs_its_command_once() {
    let (records, rises) = three_failures();
    let need = Sel::Need(None);
    let dir = scratch("crash-cursor");
    let open = |dir: &str| {
        let (mut n, fired) = appending(dir, 100, "");
        n.cfg.on = vec![need.clone()];
        n.cursor_every = Duration::MAX;
        n.retry_every = Duration::from_secs(3_600);
        n.take(Notice::Log(0, strict_log())).expect("log");
        (n, fired)
    };
    let (mut n, fired) = open(&dir);
    REFUSE.with(|refuse| *refuse.borrow_mut() = vec![("cur/", 0, libc::ENOSPC)]);
    for (off, subject, body) in records.iter().filter(|(off, ..)| *off <= rises[0]) {
        n.take(Notice::Record(0, *off, subject.clone(), body.clone()))
            .expect("judged");
    }
    assert!(
        REFUSE.with(std::cell::RefCell::take).is_empty(),
        "PRECONDITION: the page's cursor was refused"
    );
    assert_eq!(fired_offsets(&fired), [rises[0]], "PRECONDITION: it paged");
    let cursor = n.log_store.cursor(&need.key()).unwrap_or(0);
    assert!(
        cursor <= rises[0],
        "PRECONDITION: the cursor on the store, {cursor}, is not past the page"
    );
    drop(n);
    let (mut back, _) = open(&dir);
    for (off, subject, body) in records.iter().filter(|(off, ..)| *off >= cursor) {
        back.take(Notice::Record(0, *off, subject.clone(), body.clone()))
            .expect("re-read");
    }
    assert_eq!(
        fired_offsets(&fired),
        rises,
        "the restart re-read the page and ran nothing again; each later rise paged once"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A presence row: offset, session, the `attention=` it raises (`-` clears it).
type Row = (u64, &'static str, &'static str);

fn raised(&(off, sid, att): &Row) -> Notice {
    Notice::Record(
        0,
        off,
        presence_of(sid),
        format!("v=1 t=1 state=live attention={att}").into_bytes(),
    )
}

/// An `attention` follower that writes an owed cursor only at a verdict or a standing
/// change, retrying a refused write `retry` later.
fn attending(dir: &str, limit: u32, retry: Duration) -> (Notifier, PathBuf) {
    let (mut n, fired) = appending(dir, limit, "");
    n.cfg.window_ms = 3_600_000;
    n.cursor_every = Duration::MAX;
    n.retry_every = retry;
    n.take(Notice::Log(0, strict_log())).expect("log");
    (n, fired)
}

/// Re-read `rows` from the cursor on `dir`'s store, as a restart after a crash does: the
/// follower, and every offset whose command ran in either run.
fn restart(dir: &str, rows: &[Row], limit: u32) -> (Notifier, Vec<u64>) {
    let (mut back, fired) = attending(dir, limit, Duration::from_secs(60));
    let from = back.log_store.cursor(&Sel::Attention.key()).unwrap_or(1);
    for row in rows.iter().filter(|(off, ..)| *off >= from) {
        back.take(raised(row)).expect("re-read");
    }
    (back, fired_offsets(&fired))
}

fn refuse(name: &'static str, times: usize) {
    REFUSE.with(|r| *r.borrow_mut() = vec![(name, 0, libc::ENOSPC); times]);
}

/// While a standing value a record changed is unwritten, the follower judges no later
/// record, so no second change waits behind it: no one cursor write keeps the store's law
/// for two. With the cursor written at the newest of two, a crash loses the older change
/// and misses the rise after it (`miss`); at the oldest, the store holds the newer values
/// with its cursor before a record judged under the older ones, and a crash pages it again
/// (`repeat`). The retry that writes the change judges the record waiting behind it.
#[test]
fn a_record_waits_behind_an_unwritten_standing_change_so_a_crash_neither_misses_nor_repeats() {
    let retry = Duration::from_secs(60);
    let miss: [Row; 4] = [
        (1, "tzzzzzzzzza", "help"),
        (2, "tzzzzzzzzza", "-"),
        (3, "tzzzzzzzzzb", "other"),
        (4, "tzzzzzzzzza", "help"),
    ];
    for crash in [false, true] {
        let dir = scratch(&format!("standing-miss-{crash}"));
        let (mut n, fired) = attending(&dir, 100, retry);
        let base = Instant::now();
        n.clock = Some(base);
        n.take(raised(&miss[0])).expect("1 pages");
        refuse("standing", 2);
        n.take(raised(&miss[1]))
            .expect("2 clears tzzzzzzzzza, its standing value refused");
        let waits = n.take(raised(&miss[2]));
        assert_eq!(
            (waits.map_err(|e| e.raw_os_error()), fired_offsets(&fired)),
            (Err(Some(libc::ENOSPC)), vec![1]),
            "crash={crash}: tzzzzzzzzzb's raise at 3 waits behind the clear at 2"
        );
        n.clock = Some(base + retry);
        let _ = n.tick(n.now());
        assert!(
            REFUSE.with(std::cell::RefCell::take).is_empty(),
            "PRECONDITION: the clear was refused at 2 and again at its retry"
        );
        let paged = if crash {
            drop(n);
            restart(&dir, &miss, 100).1
        } else {
            n.clock = Some(base + retry * 2);
            n.tick(n.now())
                .expect("the retry writes the clear and judges 3");
            assert_eq!(
                fired_offsets(&fired),
                [1, 3],
                "the record waiting behind the clear is judged at the retry that wrote it"
            );
            n.take(raised(&miss[3])).expect("4 pages");
            fired_offsets(&fired)
        };
        assert_eq!(
            paged,
            [1, 3, 4],
            "crash={crash}: tzzzzzzzzzb's raise at 3 pages once, and tzzzzzzzzza's help, cleared at 2, \
             pages again at 4"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    let repeat: [Row; 4] = [
        (1, "tzzzzzzzzza", "help"),
        (2, "tzzzzzzzzzb", "help"),
        (3, "tzzzzzzzzza", "help"),
        (4, "tzzzzzzzzza", "-"),
    ];
    let dir = scratch("standing-repeat");
    let (mut n, fired) = attending(&dir, 100, retry);
    let base = Instant::now();
    n.clock = Some(base);
    n.take(raised(&repeat[0])).expect("1 pages");
    REFUSE.with(|r| {
        *r.borrow_mut() = vec![("standing", 0, libc::ENOSPC), ("cur/", 0, libc::ENOSPC)];
    });
    n.take(raised(&repeat[1]))
        .expect("2 pages tzzzzzzzzzb, its standing value refused");
    for row in &repeat[2..] {
        let _ = n.take(raised(row));
    }
    n.clock = Some(base + retry);
    let _ = n.tick(n.now());
    assert_eq!(
        (
            fired_offsets(&fired),
            REFUSE.with(std::cell::RefCell::take).len()
        ),
        (vec![1, 2], 0),
        "PRECONDITION: 1 and 2 paged, 3 stood, tzzzzzzzzzb's standing value was refused, and at \
         its retry a cursor"
    );
    drop(n);
    assert_eq!(
        restart(&dir, &repeat, 100).1,
        [1, 2],
        "a crash re-reads tzzzzzzzzza's republish at 3 under the value it stood under, and pages \
         nothing again"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// No cursor reaches the store while a part a record changed is unwritten, even where the
/// retry is due at once: a clear refused, a crash, and the restart re-reads the clear, so
/// the re-raise after it pages.
#[test]
fn no_cursor_passes_a_record_whose_standing_change_is_unwritten() {
    let rows: [Row; 4] = [
        (1, "tzzzzzzzzza", "help"),
        (2, "tzzzzzzzzza", "-"),
        (3, "tzzzzzzzzzc", "-"),
        (4, "tzzzzzzzzza", "help"),
    ];
    let dir = scratch("standing-hold");
    let (mut n, _) = attending(&dir, 100, Duration::ZERO);
    n.cursor_lag = 1;
    n.clock = Some(Instant::now());
    n.take(raised(&rows[0])).expect("1 pages");
    refuse("standing", 2);
    n.take(raised(&rows[1]))
        .expect("2 clears tzzzzzzzzza, refused, and its retry refused");
    assert!(
        REFUSE.with(std::cell::RefCell::take).is_empty(),
        "PRECONDITION: the clear and its retry, due at once, were refused"
    );
    drop(n);
    assert_eq!(
        restart(&dir, &rows, 100).1,
        [1, 4],
        "the restart re-read the clear at 2, so tzzzzzzzzza's help at 4 paged"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A drop whose `dropped` line is refused: its count stays off the store until the line is
/// on it (each part in its order, [`Part`]), no cursor passes it, and a crash before either
/// is written has the restart judge it again and count it once.
#[test]
fn a_drops_count_never_reaches_the_store_ahead_of_its_line_and_no_cursor_passes_it() {
    let key = Sel::Attention.key();
    let retry = Duration::from_secs(60);
    let rows: [Row; 2] = [(1, "tzzzzzzzzza", "help"), (2, "tzzzzzzzzzb", "other")];
    let on_store = |n: &Notifier| {
        (
            n.log_store
                .journal()
                .iter()
                .any(|l| l.starts_with("2 dropped ")),
            n.store.suppressed().0,
            n.log_store.cursor(&key),
        )
    };
    for crash in [false, true] {
        let dir = scratch(&format!("drop-order-{crash}"));
        let (mut n, fired) = attending(&dir, 1, retry);
        let base = Instant::now();
        n.clock = Some(base);
        n.take(raised(&rows[0])).expect("1 pages");
        refuse("journal", 2);
        n.take(raised(&rows[1])).expect("2 is judged");
        assert_eq!(
            (fired_offsets(&fired), n.counts.dropped),
            (vec![1], 1),
            "PRECONDITION: 1 paged and 2 was over the budget"
        );
        n.clock = Some(base + retry);
        let _ = n.take(Notice::Checkpoint(0, 3));
        assert!(
            REFUSE.with(std::cell::RefCell::take).is_empty(),
            "PRECONDITION: the `dropped` line was refused, and again at its retry"
        );
        assert_eq!(
            on_store(&n),
            (false, 0, Some(2)),
            "crash={crash}: the line refused, neither its count nor a cursor past it is on \
             the store"
        );
        if crash {
            drop(n);
            let (back, paged) = restart(&dir, &rows, 1);
            assert_eq!(
                (paged, on_store(&back)),
                (vec![1], (true, 1, Some(3))),
                "the restart judged 2 again: dropped and counted once"
            );
        } else {
            n.clock = Some(base + retry * 2);
            n.tick(n.now()).expect("the retry");
            assert_eq!(
                on_store(&n),
                (true, 1, Some(3)),
                "the retry wrote the line, its count, then the cursor past it"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// What a run left: the offsets whose command ran (each once, however often), the
/// journal's verdicts, the suppressed count and the stamps spent, as the store holds them.
type Left = (Vec<u64>, Vec<(u64, String)>, u64, usize);

/// Every kill point of a run, in the two layouts a store has: the root's log, whose verdict
/// and what it spends or counts are one step of one journal, so no kill falls between them and
/// a run with no restore loses nothing; and a log's own store under `logs/`, whose verdicts go
/// to its own journal ahead of the stamps and the count in the root's, where a kill between a
/// page's `firing` line and its stamp, or a drop's line and its count, is what the restore
/// finishes (the NEGATIVE CONTROL: without it, those kills under-spend or under-count).
#[test]
fn a_kill_between_a_verdict_and_its_count_neither_charges_nor_counts_a_record_twice() {
    for own_store in [false, true] {
        kill_sweep(own_store);
    }
}

fn kill_sweep(own_store: bool) {
    let key = Sel::Attention.key();
    let raise = |sid: &str| {
        (
            presence_of(sid),
            format!("v=1 t=1 attention=help-{sid}").into_bytes(),
        )
    };
    let quiet = || (presence_of("tzzzzzzzzzq"), b"v=1 t=1 state=live".to_vec());
    let records: Vec<(u64, String, Vec<u8>)> = [
        quiet(),
        raise("tzzzzzzzzza"),
        quiet(),
        raise("tzzzzzzzzzb"),
        raise("tzzzzzzzzzc"),
        raise("tzzzzzzzzzd"),
        raise("tzzzzzzzzze"),
        quiet(),
    ]
    .into_iter()
    .zip(1..)
    .map(|((subject, body), off)| (off, subject, body))
    .collect();
    let open = |dir: &str| {
        if own_store {
            let _ = Store::open(dir)
                .expect("open")
                .for_log(
                    Some(astream_broker::LogIdentity {
                        id: [8; 16],
                        generation: 0,
                    }),
                    None,
                    &Steps::default(),
                )
                .expect("the root's store claimed by another log");
        }
        let (mut n, fired) = appending(dir, 3, "");
        n.cfg.window_ms = 3_600_000;
        n.cursor_every = Duration::MAX;
        n.take(Notice::Log(0, strict_log())).expect("log");
        n.scope_to(&strict_log())
            .expect("the log's store taken up, so a restart's restore is the test's to skip");
        (n, fired)
    };
    let feed = |n: &mut Notifier| -> io::Result<()> {
        let from = n.log_store.cursor(&key).unwrap_or(0);
        for (off, subject, body) in records.iter().filter(|(off, ..)| *off >= from) {
            n.take(Notice::Record(0, *off, subject.clone(), body.clone()))?;
        }
        Ok(())
    };
    let left = |n: &Notifier, fired: &Path| -> Left {
        let mut paged = fired_offsets(fired);
        paged.sort_unstable();
        paged.dedup();
        let verdicts = records
            .iter()
            .filter_map(|(off, ..)| n.verdict(*off).map(|v| (*off, v.to_string())))
            .collect();
        (
            paged,
            verdicts,
            n.store.suppressed().0,
            n.store.stamps().len(),
        )
    };

    let dir = scratch(&format!("kill-sweep-{own_store}"));
    let (mut n, fired) = open(&dir);
    PUT_FAULT.with(|left| left.set(Some(u32::MAX)));
    feed(&mut n).expect("the run no kill stops");
    let writes = u32::MAX - PUT_FAULT.with(std::cell::Cell::take).expect("counted");
    let whole = left(&n, &fired);
    assert_eq!(
        whole,
        (
            vec![2, 4, 5],
            vec![
                (2, "fired".into()),
                (4, "fired".into()),
                (5, "fired".into()),
                (6, "dropped".into()),
                (7, "dropped".into()),
            ],
            2,
            3
        ),
        "PRECONDITION: a budget of 3 pages 2, 4 and 5 and drops 6 and 7"
    );
    drop(n);
    let _ = std::fs::remove_dir_all(&dir);

    let mut unrestored = Vec::new();
    for kill in 0..writes {
        for restore in [true, false] {
            let dir = scratch(&format!("kill-sweep-{own_store}-{kill}-{restore}"));
            let (mut n, _) = open(&dir);
            PUT_FAULT.with(|left| left.set(Some(kill)));
            let _ = feed(&mut n);
            assert_eq!(
                PUT_FAULT.with(std::cell::Cell::take),
                Some(0),
                "PRECONDITION: the run reached write {kill} of {writes}, and wrote nothing \
                 past it, as if it died there"
            );
            drop(n);
            let (mut back, fired) = open(&dir);
            back.restore_due = restore;
            feed(&mut back).expect("the restart reads on from its cursor");
            let after = left(&back, &fired);
            if restore {
                assert_eq!(
                    after, whole,
                    "killed before write {kill} of {writes}: the restart pages and drops \
                     what the whole run did, spends its budget once a page and counts each \
                     drop once"
                );
            } else if after != whole {
                unrestored.push(kill);
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
    if own_store {
        assert_eq!(
            unrestored.len(),
            5,
            "NEGATIVE CONTROL: in a log's own store, with no restore, a kill between each \
             page's `firing` line and its stamp (3) or each drop's line and its count (2) \
             under-spends or under-counts: {unrestored:?}"
        );
    } else {
        assert_eq!(
            unrestored,
            Vec::<u32>::new(),
            "in the root's store a verdict and what it spends or counts are one step: no kill \
             falls between them"
        );
    }
}

#[test]
fn the_flush_tally_tells_every_flush_a_carried_generation_asks_for() {
    let dir = scratch("tally-carry");
    let store = Store::open(&dir).expect("open");
    let g = |generation| astream_broker::LogIdentity {
        id: [7; 16],
        generation,
    };
    store.for_log(Some(g(0)), None, &Steps::default()).unwrap();
    let one = store.for_log(Some(g(1)), None, &Steps::default()).unwrap();
    one.set_cursor("attention", 9).unwrap();
    one.set_journal(&["2 fired rc=0".to_string()], Flush::Ordered)
        .unwrap();
    one.set_standing(&[("/f/f1/pub/n-a/s-x/presence".into(), "help".into())])
        .unwrap();

    let recording = aterm_durable::seam::Recording::start();
    NOTED.with(|noted| noted.borrow_mut().clear());
    let two = store
        .for_log(
            Some(g(2)),
            Some(&lineage_of(&[(1, 3), (2, 5)])),
            &Steps::default(),
        )
        .unwrap();
    let noted = NOTED.with(|noted| noted.take());
    assert_eq!(two.cursor("attention"), Some(5), "PRECONDITION: carried");
    assert_eq!(
        noted.iter().filter(|f| f.is_durable()).count(),
        2,
        "the carried store's root, then `logs/` once it is renamed in, each flushed in full: \
         {noted:?}"
    );
    assert_eq!(
        noted,
        recording.take(),
        "the tally tells each flush the carry asked of aterm_durable"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE EXEC STAMPS TELL EACH FRESH PAGE ONCE ([`EXEC_STAMPS_SEAM`], the fabric bench's
/// split of a page): after the notice, under the page's offset, in the order the page ran
/// (`firing` written, the command exited, `fired` written); a tell with no page run since
/// writes nothing.
#[test]
fn the_exec_stamps_tell_each_fresh_page_once_in_the_order_it_ran() {
    let dir = scratch("exec-stamps");
    let (mut n, fired) = appending(&dir, 100, "");
    let path = n.store.root.join(EXEC_STAMPS);
    n.exec_stamps = Some(ExecStamps {
        origin: Instant::now(),
        untold: None,
    });
    for off in [1, 2] {
        feed(&mut n, off, &generated_sid(off), "v=1 t=1 attention=help");
        n.tell();
        assert_eq!(
            fired_offsets(&fired).last(),
            Some(&off),
            "PRECONDITION: paged"
        );
        let told = std::fs::read_to_string(&path).expect("the stamps are told");
        let us = |key| {
            crate::fabric::kv(&told, key)
                .and_then(crate::parse_uint::<u64>)
                .unwrap_or_else(|| panic!("no {key}= in {told:?}"))
        };
        assert!(told.starts_with("notify-exec v=1 "), "{told:?}");
        assert_eq!(us("off"), off, "{told:?}");
        assert!(
            us("armed") <= us("exited") && us("exited") <= us("fired"),
            "in the order the page ran: {told:?}"
        );
        std::fs::remove_file(&path).expect("take the told stamps");
        n.tell();
        assert!(!path.exists(), "told once");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`Notifier::exec`] of `command` for an ask, its timeout `timeout`.
#[cfg(unix)]
fn exec_of(dir: &str, command: &str, timeout: Duration) -> String {
    let hit = Sel::Ask("h-andrew".into())
        .hit(
            "f1",
            7,
            "/f/f1/in/p/h-andrew/n-a/ask",
            b"v=1 t=1 from=tzzzzzzzzzb text=x",
        )
        .expect("an ask to h-andrew");
    let mut c = cfg(dir);
    c.exec = command.into();
    c.exec_timeout = timeout;
    Notifier::new(c).expect("open").exec(&hit, false, 0)
}

/// A COMMAND'S END IS ITS VERDICT (D07: the wait on the kernel's word of the exit): an
/// exit, a non-zero exit and a signal each told as it ended; a command past its timeout
/// killed with its whole group, so a member it left running is gone too, `rc=timeout`.
#[cfg(unix)]
#[test]
fn exec_tells_how_its_command_ended_and_kills_one_past_its_timeout_with_its_group() {
    let dir = scratch("exec-ends");
    std::fs::create_dir_all(&dir).expect("scratch");
    let must = aterm_spec::wait::MUST_HAPPEN;
    for (command, verdict) in [
        ("true", "rc=0"),
        ("exit 3", "rc=3"),
        ("kill -9 $$", "rc=signalled"),
    ] {
        assert_eq!(exec_of(&dir, command, must), verdict, "{command}");
    }
    let group = std::rc::Rc::new(std::cell::Cell::new(0));
    BEFORE_EXIT_WAIT.with(|seam| {
        let group = group.clone();
        *seam.borrow_mut() = Some(Box::new(move |child| group.set(child.id())));
    });
    let verdict = exec_of(&dir, "sleep 600 & wait", Duration::from_millis(100));
    BEFORE_EXIT_WAIT.with(|seam| seam.borrow_mut().take());
    assert_eq!(verdict, "rc=timeout");
    aterm_spec::wait::until_within(must, "the timed-out command's group is gone", || {
        (aterm_uds::process::group_presence(group.get())
            == aterm_uds::process::ProcessPresence::Absent)
            .then_some(())
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// AN EXIT AHEAD OF ITS WAIT IS REAPED AT ONCE (D07): the command has exited, unreaped,
/// before [`Notifier::exec`] registers its exit wait (the seam waits on the exit without
/// reaping it), and Darwin's kqueue will not watch a zombie, so that wait has no word to
/// give and could only sleep its fallback ticks. Each turn reaps first, so the page ends
/// `rc=0` without a single wait turn, under a one-minute hang detector (its timeout is ten
/// minutes). NEGATIVE CONTROL (scratch): a loop that waits before it reaps ends `rc=0`
/// too, after one fallback tick (`exitwatch::exit_fd` of the zombie read none), so the
/// count of wait turns is what tells it apart; the hang detector holds a wait that would
/// sleep to the timeout.
#[cfg(unix)]
#[test]
fn a_command_that_exits_before_its_exit_wait_registers_is_reaped_at_once() {
    let dir = scratch("exit-first");
    std::fs::create_dir_all(&dir).expect("scratch");
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn({
        let dir = dir.clone();
        move || {
            BEFORE_EXIT_WAIT.with(|seam| {
                *seam.borrow_mut() = Some(Box::new(|child| {
                    child.wait_exited().expect("the command exits, unreaped");
                }));
            });
            let verdict = exec_of(&dir, "true", Duration::from_secs(600));
            let _ = tx.send((verdict, EXIT_WAITS.with(std::cell::Cell::get)));
        }
    });
    let (verdict, turns) = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("HANG: an exit ahead of the exit wait was not reaped");
    assert_eq!(verdict, "rc=0");
    assert_eq!(turns, 0, "reaped before any wait on its exit");
    worker.join().expect("the exec thread");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A RUNNING COMMAND IS REAPED ON THE KERNEL'S WORD OF ITS EXIT (D07), by a count of wait
/// turns, not a clock: a command alive about 300 ms ends in at most three. One is the turn the
/// kernel's word ends. Any others are fallback ticks of 1 and 2 ms, for an exit reported
/// just before the command can be reaped. The count does not grow with the command's life,
/// where ticks with no descriptor (1 ms doubling to [`EXEC_FALLBACK_TICK`]) take about 33.
/// The seam first makes an exit descriptor of its own for the live command: under a Linux
/// translator `pidfd_open` returns ENOSYS (NOT RUN), and on any other Darwin or Linux host
/// a missing descriptor fails the test. NEGATIVE CONTROL (scratch): a loop that sleeps
/// `left.min(10 ms)` in place of `exit.wait(left)` fails it, `25 wait turns for one exit`.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_running_command_is_reaped_on_the_kernels_word_of_its_exit() {
    let dir = scratch("exit-word");
    std::fs::create_dir_all(&dir).expect("scratch");
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn({
        let dir = dir.clone();
        move || {
            let watched = std::rc::Rc::new(std::cell::Cell::new(None));
            BEFORE_EXIT_WAIT.with(|seam| {
                let watched = watched.clone();
                *seam.borrow_mut() = Some(Box::new(move |child| {
                    let fd = aterm_uds::exitwatch::exit_fd(child.id());
                    let errno = std::io::Error::last_os_error().raw_os_error();
                    watched.set(Some(fd.map(drop).ok_or(errno)));
                }));
            });
            let verdict = exec_of(&dir, "sleep 0.3", Duration::from_secs(600));
            let turns = EXIT_WAITS.with(std::cell::Cell::get);
            let _ = tx.send((watched.get(), verdict, turns));
        }
    });
    let (watched, verdict, turns) = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("HANG: a running command's exit was not reaped");
    worker.join().expect("the exec thread");
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(errno) = watched.expect("the seam ran") {
        #[cfg(target_os = "linux")]
        if errno == Some(38) {
            if let Some(translator) = aterm_uds::process::translator() {
                println!(
                    "NOT RUN under {translator}: notify::tests::a_running_command_is_reaped_on_the_kernels_word_of_its_exit: pidfd_open returned ENOSYS"
                );
                return;
            }
        }
        panic!("no exit descriptor for a live command: errno {errno:?}");
    }
    assert_eq!(verdict, "rc=0");
    assert!(turns <= 3, "{turns} wait turns for one exit");
}

fn ordered_broker(log: &Path) -> Option<astream_broker::Broker> {
    let mut opts = crate::durability::Durability::Ordered.options();
    opts.durability = astream_broker::Durability::Ordered { horizon: None };
    let broker =
        astream_broker::Broker::open_opts(log, opts, || Ok(astream_broker::Prepared::Nothing))
            .expect("open");
    (broker.served_durability().served == astream_broker::Tier::Ordered).then_some(broker)
}

#[cfg(unix)]
#[test]
fn an_ordered_broker_pages_a_record_only_once_it_is_durable() {
    for wait in [true, false] {
        let dir = scratch(&format!("ord-{wait}"));
        let base = PathBuf::from(&dir);
        std::fs::create_dir_all(&base).expect("scratch");
        let log = base.join("o.log");
        let sock = base.join("o.sock");
        let Some(broker) = ordered_broker(&log) else {
            eprintln!("ordered is served strict here: nothing waits, nothing to prove");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        let _server = broker.serve(&sock).unwrap();
        let fired = base.join("fired");
        let mut c = cfg(&dir);
        c.broker = sock.to_string_lossy().into_owned();
        c.since = Since::Start;
        c.limit = 10;
        c.on = vec![Sel::Attention, Sel::Halt];
        c.exec = format!(
            "printf '%s %s\\n' \"$ATERM_NOTIFY_ON\" \"$ATERM_NOTIFY_OFFSET\" >> {}",
            fired.display()
        );
        let run = || {
            let mut n = Notifier::new(c.clone()).expect("open");
            n.wait_durable = wait;
            n.catch_up().expect("catch up");
            (
                std::fs::read_to_string(&fired).unwrap_or_default(),
                n.log_store.cursor(&Sel::Attention.key()),
            )
        };
        let (mut conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
        let raised = conn
            .publish(
                7,
                1,
                "/f/f1/pub/n-a/tzzzzzzzzzx/presence",
                b"v=1 t=1 attention=needs%20a%20key",
            )
            .unwrap()
            .0;
        let halt = conn
            .publish(
                8,
                1,
                "/f/f1/fleet/h-x/halt",
                b"v=1 t=1 state=on reason=stop",
            )
            .unwrap()
            .0;
        assert!(broker.durable_head() > halt, "the halt is escalated");
        let waits = conn
            .publish(
                7,
                2,
                "/f/f1/pub/n-b/tzzzzzzzzzy/presence",
                b"v=1 t=1 attention=disk",
            )
            .unwrap()
            .0;
        assert!(raised < halt && broker.durable_head() <= waits);
        let (first, at) = run();
        if wait {
            assert_eq!(
                first,
                format!("attention {raised}\nhalt {halt}\n"),
                "only what is durable pages"
            );
            assert_eq!(at, Some(waits), "the cursor stays AT the row that waits");
            broker.flush_durable().expect("a full flush");
            assert!(broker.durable_head() > waits);
            let (second, at) = run();
            assert_eq!(
                second,
                format!("attention {raised}\nhalt {halt}\nattention {waits}\n"),
                "durable now: paged, once"
            );
            assert_eq!(at, Some(waits + 1));
            assert_eq!(run().0, second, "and never again");
        } else {
            assert_eq!(
                first,
                format!("attention {raised}\nattention {waits}\nhalt {halt}\n"),
                "NEGATIVE CONTROL: paged before any full flush covered it"
            );
        }
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn pump(
    n: &mut Notifier,
    rx: &mpsc::Receiver<Notice>,
    what: &str,
    done: impl Fn(&Notifier) -> bool,
) -> &'static str {
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let mut last = "none";
    while !done(n) {
        assert!(Instant::now() < deadline, "{what}: never happened");
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(notice) => {
                last = match notice {
                    Notice::Record(..) => "record",
                    Notice::Warm(..) => "warm",
                    Notice::Checkpoint(..) => "checkpoint",
                    Notice::Start(..) => "start",
                    Notice::Log(..) => "log",
                    Notice::Durable(..) => "durable",
                };
                n.take(notice).expect("take");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!("{what}: the reader ended"),
        }
    }
    last
}

#[cfg(unix)]
#[test]
fn in_follow_mode_the_horizon_alone_pages_an_attention_row() {
    for wait in [true, false] {
        let dir = scratch(&format!("horizon-{wait}"));
        let base = PathBuf::from(&dir);
        std::fs::create_dir_all(&base).expect("scratch");
        let log = base.join("h.log");
        let sock = base.join("h.sock");
        let broker = astream_broker::Broker::open_opts(
            &log,
            crate::durability::Durability::Ordered.options(),
            || Ok(astream_broker::Prepared::Nothing),
        )
        .expect("open");
        let served = broker.served_durability();
        if served.served != astream_broker::Tier::Ordered {
            eprintln!("ordered is served strict here: nothing waits, nothing to prove");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert_eq!(served.horizon_ms, Some(100), "aterm's own horizon");
        let mut server = broker.serve(&sock).unwrap();
        let fired = base.join("fired");
        let mut c = cfg(&dir);
        c.broker = sock.to_string_lossy().into_owned();
        c.since = Since::Start;
        c.once = false;
        c.limit = 10;
        c.exec = format!(
            "printf '%s\\n' \"$ATERM_NOTIFY_OFFSET\" >> {}",
            fired.display()
        );
        let mut n = Notifier::new(c.clone()).expect("open");
        n.wait_durable = wait;
        let (tx, rx) = notice_handoff();
        let follower = {
            let hold = std::sync::Arc::clone(&n.hold);
            std::thread::spawn(move || reader(0, &Sel::Attention, &c, &hold, &tx))
        };
        pump(&mut n, &rx, "the reader's subscription", |_| {
            broker.stats().parked_tails == 1
        });
        let start = broker.flush_counts().full;
        let (mut conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
        let raised = conn
            .publish(
                7,
                1,
                "/f/f1/pub/n-a/tzzzzzzzzzx/presence",
                b"v=1 t=1 attention=needs%20a%20key",
            )
            .unwrap()
            .0;
        let by = pump(&mut n, &rx, "the page", |_| {
            !fired_offsets(&fired).is_empty()
        });
        assert_eq!(fired_offsets(&fired), [raised], "{wait}: paged once");
        if wait {
            assert_eq!(by, "durable", "the durable mark is what paged it");
            assert!(broker.durable_head() > raised);
            assert_eq!(
                broker.flush_counts().full,
                start + 1,
                "one full flush, the horizon's"
            );
            assert_eq!(n.held_bytes, 0);
            assert_eq!(n.log_store.cursor(&Sel::Attention.key()), Some(raised + 1));
        } else {
            assert_eq!(by, "record", "NEGATIVE CONTROL: paged as it was read");
        }
        drop(rx);
        conn.publish(
            7,
            2,
            "/f/f1/pub/n-a/tzzzzzzzzzy/presence",
            b"v=1 t=1 attention=disk",
        )
        .unwrap();
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the reader ends", || {
            follower.is_finished().then_some(())
        });
        follower.join().expect("the reader");
        drop(conn);
        server.shutdown();
        drop(broker);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn lineage_of(steps: &[(u64, u64)]) -> astream_broker::Lineage {
    astream_broker::Lineage::from_steps(steps).expect("rising generations, at most MAX")
}

#[test]
fn the_cursor_never_goes_backwards() {
    let dir = scratch("cursor");
    let store = Store::open(&dir).expect("open");
    store.set_cursor("attention", 40).expect("write");
    store.set_cursor("attention", 9).expect("write");
    assert_eq!(store.cursor("attention"), Some(40));
    store.set_cursor("attention", 41).expect("write");
    assert_eq!(store.cursor("attention"), Some(41));
    assert_eq!(store.cursor("halt"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn selectors_parse_and_spell_a_filename() {
    assert_eq!(Sel::parse("attention"), Ok(Sel::Attention));
    assert_eq!(Sel::parse("halt"), Ok(Sel::Halt));
    assert_eq!(Sel::parse("ask:h-andrew"), Ok(Sel::Ask("h-andrew".into())));
    assert_eq!(Sel::Ask("h-andrew".into()).key(), "ask-to.h-andrew");
    assert_eq!(
        Sel::Ask("h-andrew".into()).filter("f1"),
        "/f/f1/in/p/h-andrew/*/ask"
    );
    for bad in ["ask:../../etc", "ask:", "ask:H-Andrew", "wake", "ask"] {
        assert!(Sel::parse(bad).is_err(), "{bad}");
    }
}

/// The bodies a session log's mirror lands on the bus, take by take, as the bridge takes
/// them ([`aterm_slog::Log::bus_copy`]).
fn mirrored(log: &mut aterm_slog::Log) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        let take = log
            .bus_copy(64, aterm_slog::Stamp::new(9, 9))
            .encode(aterm_slog::Tier::Wire, &|_| None);
        if take.slots.is_empty() {
            return out;
        }
        out.extend(take.slots.iter().map(|s| s.body.clone()));
        log.land(take.slots.last().expect("a slot").at)
            .expect("lands");
        if !take.more {
            return out;
        }
    }
}

/// A session log that a test drives through needs: `failure` from a shell command's
/// failed run, lifted by the next one of its kind, and `question` from a step.
struct Needs {
    log: aterm_slog::Log,
    ms: u64,
}

impl Needs {
    fn open() -> Self {
        Self::in_space(1)
    }

    fn in_space(ep: u32) -> Self {
        let mut log = aterm_slog::Log::new(ep);
        log.open_session(
            aterm_slog::Kind::Spawn,
            aterm_slog::Src::Aterm,
            aterm_slog::Conf::Exact,
            aterm_slog::Actor::Aterm,
            aterm_slog::Fields::new(),
            aterm_slog::Stamp::new(1_760_000_000_000, 0),
        )
        .expect("opens");
        Self { log, ms: 0 }
    }

    fn at(&mut self) -> aterm_slog::Stamp {
        self.ms += 1;
        aterm_slog::Stamp::new(1_760_000_000_000 + self.ms, self.ms)
    }

    fn cmd_open(&mut self) -> aterm_slog::Node {
        let at = self.at();
        self.log
            .open(
                aterm_slog::Open::new(
                    aterm_slog::Kind::Cmd,
                    aterm_slog::Src::Shell,
                    aterm_slog::Conf::Strong,
                ),
                at,
            )
            .expect("a command")
    }

    fn cmd_close(&mut self, cmd: aterm_slog::Node, out: aterm_slog::RunOut) {
        let at = self.at();
        self.log
            .close(
                cmd,
                aterm_slog::Close::new(
                    aterm_slog::Out::Run(out),
                    aterm_slog::Src::Shell,
                    aterm_slog::Conf::Exact,
                ),
                at,
            )
            .expect("closes");
    }

    fn question(&mut self) -> aterm_slog::Node {
        let turn = self.turn();
        self.ask(turn)
    }

    /// An agent run and a turn under it: the turn.
    fn turn(&mut self) -> aterm_slog::Node {
        let at = self.at();
        let run = self
            .log
            .open(
                aterm_slog::Open::new(
                    aterm_slog::Kind::Agent,
                    aterm_slog::Src::Status,
                    aterm_slog::Conf::Strong,
                ),
                at,
            )
            .expect("an agent run");
        let at = self.at();
        self.log
            .open(
                aterm_slog::Open::new(
                    aterm_slog::Kind::Turn,
                    aterm_slog::Src::Screen,
                    aterm_slog::Conf::Strong,
                )
                .under(run),
                at,
            )
            .expect("a turn")
    }

    /// A question asked under `turn`.
    fn ask(&mut self, turn: aterm_slog::Node) -> aterm_slog::Node {
        let at = self.at();
        self.log
            .open(
                aterm_slog::Open::new(
                    aterm_slog::Kind::Question,
                    aterm_slog::Src::Screen,
                    aterm_slog::Conf::Strong,
                )
                .under(turn),
                at,
            )
            .expect("a question")
    }

    /// The question `step` answered, which lifts its need.
    fn answer(&mut self, step: aterm_slog::Node) {
        let at = self.at();
        self.log
            .close(
                step,
                aterm_slog::Close::new(
                    aterm_slog::Out::Step(aterm_slog::StepOut::Answered),
                    aterm_slog::Src::Screen,
                    aterm_slog::Conf::Strong,
                ),
                at,
            )
            .expect("answered");
    }

    /// Mail delivered to the session, which moves its counts: the mirror owes a snapshot.
    fn mail(&mut self) {
        let at = self.at();
        self.log
            .mark(
                aterm_slog::Mark::new(
                    aterm_slog::Kind::Mail,
                    aterm_slog::Src::Fabric,
                    aterm_slog::Conf::Exact,
                )
                .fields(
                    aterm_slog::Fields::new()
                        .word(aterm_slog::Key::Dir, "in")
                        .expect("dir"),
                ),
                at,
            )
            .expect("mail");
    }

    /// A mark that moves no need (a handoff naming no epoch).
    fn quiet(&mut self) {
        let at = self.at();
        self.log
            .mark(
                aterm_slog::Mark::new(
                    aterm_slog::Kind::Handoff,
                    aterm_slog::Src::Aterm,
                    aterm_slog::Conf::Exact,
                )
                .at(aterm_slog::Node::SESSION)
                .by(aterm_slog::Actor::Aterm),
                at,
            )
            .expect("a handoff mark");
    }

    /// The session's close (its shell exited), which raises `exited` and drops every other
    /// need.
    fn exit(&mut self) {
        let at = self.at();
        self.log
            .close_session(
                aterm_slog::Close::new(
                    aterm_slog::Out::Session(aterm_slog::SessionOut::ShellExit),
                    aterm_slog::Src::Shell,
                    aterm_slog::Conf::Exact,
                ),
                at,
            )
            .expect("closes");
    }

    /// A clean quit's close of the session (aterm-gui's `SessionLog::close_at_quit`, fix
    /// unit Q1): `out=app-quit by=aterm`, which closes every node the session holds open.
    fn quit(&mut self) {
        let at = self.at();
        self.log
            .close_session(
                aterm_slog::Close::new(
                    aterm_slog::Out::Session(aterm_slog::SessionOut::AppQuit),
                    aterm_slog::Src::Aterm,
                    aterm_slog::Conf::Exact,
                )
                .by(aterm_slog::Actor::Aterm),
                at,
            )
            .expect("the quit closes");
    }

    fn bodies(&mut self) -> Vec<String> {
        mirrored(&mut self.log)
    }
}

const NEED_FACE: &str = "/f/f1/pub/n-a/tzzzzzzzzzx/ev";

#[cfg(unix)]
#[test]
fn a_need_pages_once_per_rise_and_a_restart_pages_nothing_again() {
    let dir = scratch("need");
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let broker = crate::test_broker(base.join("n.log")).unwrap();
    let sock = base.join("n.sock");
    let mut server = broker.serve(&sock).unwrap();
    let (mut conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
    let mut seq = 0;
    let mut publish = |bodies: Vec<String>| -> Vec<(u64, String)> {
        bodies
            .into_iter()
            .map(|b| {
                seq += 1;
                let off = conn.publish(7, seq, NEED_FACE, b.as_bytes()).unwrap().0;
                (off, b)
            })
            .collect()
    };
    let mut s = Needs::open();
    let first = s.cmd_open();
    s.cmd_close(first, aterm_slog::RunOut::Fail);
    let rise = publish(s.bodies());
    let second = s.cmd_open();
    let standing = publish(s.bodies());
    s.cmd_close(second, aterm_slog::RunOut::Ok);
    let clear = publish(s.bodies());
    let third = s.cmd_open();
    s.cmd_close(third, aterm_slog::RunOut::Fail);
    let again = publish(s.bodies());
    s.question();
    let moved = publish(s.bodies());
    let fail_close = |page: &[(u64, String)]| {
        page.iter()
            .find(|(_, b)| b.contains(" op=close ") && b.contains("out=fail"))
            .map(|(off, _)| *off)
            .expect("the failed run's close")
    };
    let (rose, rose_again) = (fail_close(&rise), fail_close(&again));
    let asked = moved
        .iter()
        .find(|(_, b)| b.contains(" op=open ") && b.contains("k=question"))
        .map(|(off, _)| *off)
        .expect("the question's open");
    assert!(
        !standing.is_empty() && !clear.is_empty(),
        "a repeat and a clear on the bus"
    );

    let run = |tag: &str, on: Sel| -> Vec<String> {
        let state = base.join(tag);
        let fired = base.join(format!("{tag}.fired"));
        let mut c = cfg(&state.to_string_lossy());
        c.broker = sock.to_string_lossy().into_owned();
        c.since = Since::Start;
        c.limit = 100;
        c.on = vec![on];
        c.exec = format!(
            "printf '%s %s %s %s %s\\n' \"$ATERM_NOTIFY_OFFSET\" \"$ATERM_NOTIFY_ON\" \
             \"$ATERM_NOTIFY_NEED\" \"$ATERM_NOTIFY_SID\" \"$ATERM_NOTIFY_NODE\" >> {}",
            fired.display()
        );
        Notifier::new(c)
            .expect("open")
            .catch_up()
            .expect("catch up");
        std::fs::read_to_string(&fired)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    };
    let line = |off: u64, word: &str| format!("{off} need {word} tzzzzzzzzzx n-a");
    let failure = Sel::Need(Some(vec!["failure"]));
    assert_eq!(
        run("failure", failure.clone()),
        [line(rose, "failure"), line(rose_again, "failure")],
        "need:failure pages each rise to failure once"
    );
    assert_eq!(
        run("any", Sel::Need(None)),
        [
            line(rose, "failure"),
            line(rose_again, "failure"),
            line(asked, "question")
        ],
        "need pages each rise, to any word"
    );

    for _ in 0..=aterm_slog::SNAP_EVERY {
        s.quiet();
    }
    let after = publish(s.bodies());
    assert!(
        after.iter().any(|(_, b)| b.contains(" op=snap ")),
        "a snapshot of the standing needs reaches the bus after the restart"
    );
    assert_eq!(
        run("failure", failure).len(),
        2,
        "a restart pages nothing again"
    );
    assert_eq!(
        run("any", Sel::Need(None)).len(),
        3,
        "nor for a need standing through the restart and a snapshot"
    );
    server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_following_need_reader_folds_below_its_cursor_before_it_judges() {
    let dir = scratch("need-follow");
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let broker = crate::test_broker(base.join("f.log")).unwrap();
    let sock = base.join("f.sock");
    let mut server = broker.serve(&sock).unwrap();
    let (mut conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
    let mut seq = 0;
    let mut publish = |conn: &mut Conn, face: &str, bodies: Vec<String>| -> Option<u64> {
        let mut rose = None;
        for b in bodies {
            seq += 1;
            let off = conn.publish(7, seq, face, b.as_bytes()).unwrap().0;
            if b.contains(" op=close ") && b.contains("out=fail") {
                rose = Some(off);
            }
        }
        rose
    };
    let failing = |s: &mut Needs| {
        let cmd = s.cmd_open();
        s.cmd_close(cmd, aterm_slog::RunOut::Fail);
        s.bodies()
    };
    let mut x = Needs::open();
    let rose = publish(&mut conn, NEED_FACE, failing(&mut x)).expect("a rise");
    let fired = base.join("fired");
    let mut c = cfg(&dir);
    c.broker = sock.to_string_lossy().into_owned();
    c.since = Since::Start;
    c.limit = 10;
    c.on = vec![Sel::Need(None)];
    c.exec = format!(
        "printf '%s\\n' \"$ATERM_NOTIFY_OFFSET\" >> {}",
        fired.display()
    );
    Notifier::new(c.clone())
        .expect("open")
        .catch_up()
        .expect("catch up");
    assert_eq!(fired_offsets(&fired), [rose]);

    c.once = false;
    let mut n = Notifier::new(c.clone()).expect("reopen");
    let (tx, rx) = notice_handoff();
    let follower = {
        let hold = std::sync::Arc::clone(&n.hold);
        std::thread::spawn(move || reader(0, &Sel::Need(None), &c, &hold, &tx))
    };
    pump(&mut n, &rx, "the reader's subscription", |_| {
        broker.stats().parked_tails == 1
    });
    for _ in 0..=aterm_slog::SNAP_EVERY {
        x.quiet();
    }
    let quiet = x.bodies();
    assert!(quiet.iter().any(|b| b.contains(" op=snap ")), "a snapshot");
    publish(&mut conn, NEED_FACE, quiet);
    let mut y = Needs::open();
    let other =
        publish(&mut conn, "/f/f1/pub/n-a/tzzzzzzzzzy/ev", failing(&mut y)).expect("a rise");
    pump(&mut n, &rx, "the other session's page", |_| {
        fired_offsets(&fired).contains(&other)
    });
    assert_eq!(
        fired_offsets(&fired),
        [rose, other],
        "the standing failure, reseeded by a snapshot past the cursor, pages nothing again"
    );
    drop(rx);
    publish(&mut conn, NEED_FACE, {
        x.quiet();
        x.bodies()
    });
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the reader ends", || {
        follower.is_finished().then_some(())
    });
    follower.join().expect("the reader");
    drop(conn);
    server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The words of the pairs standing on `face` ([`Notifier::pairs_of`]).
fn standing_words(n: &Notifier, face: &str) -> Vec<&'static str> {
    n.pairs_of(face).into_iter().map(|p| p.word).collect()
}

#[test]
fn an_exited_session_clears_its_standing_needs_and_a_will_or_a_relaunch_does_not() {
    for will in ["away", "gone"] {
        let dir = scratch(&format!("need-gone-{will}"));
        let (mut n, fired) = appending(&dir, 100, "");
        n.cfg.on = vec![Sel::Need(None)];
        let sel = Sel::Need(None);
        let mut off = 0;
        let mut feed = |n: &mut Notifier, subject: &str, body: &[u8]| {
            off += 1;
            n.on_record(&sel, off, subject, body).expect("on_record");
            off
        };
        let faces = ["tzzzzzzzzzx", "tzzzzzzzzzy"].map(|sid| format!("/f/f1/pub/n-a/{sid}/ev"));
        let mut rises = Vec::new();
        for face in &faces {
            let mut s = Needs::open();
            let cmd = s.cmd_open();
            s.cmd_close(cmd, aterm_slog::RunOut::Fail);
            for b in s.bodies() {
                let at = feed(&mut n, face, b.as_bytes());
                if b.contains("out=fail") {
                    rises.push(at);
                }
            }
        }
        assert_eq!(fired_offsets(&fired), rises, "each session's failure pages");
        let standing = |n: &Notifier| faces.clone().map(|f| standing_words(n, &f));
        let both = [vec!["failure"], vec!["failure"]];
        assert_eq!(standing(&n), both);

        feed(
            &mut n,
            "/f/f1/pub/n-a/node/presence",
            b"v=1 t=1 state=live inc=2",
        );
        feed(
            &mut n,
            "/f/f1/pub/n-a/tzzzzzzzzzx/presence",
            b"v=1 t=1 inc=1 state=live",
        );
        assert_eq!(
            standing(&n),
            both,
            "{will}: a relaunch whose bridge has not republished the session is not its end"
        );
        feed(
            &mut n,
            "/f/f1/pub/n-a/tzzzzzzzzzx/presence",
            b"v=1 t=2 inc=2 state=exited",
        );
        assert_eq!(
            standing(&n),
            [vec![], vec!["failure"]],
            "{will}: an exited session's needs clear"
        );
        feed(
            &mut n,
            "/f/f1/pub/n-a/node/presence",
            format!("v=1 t=3 state={will} inc=2 fabric=disconnected").as_bytes(),
        );
        assert_eq!(
            standing(&n),
            [vec![], vec!["failure"]],
            "{will}: a node's will keeps its sessions' needs standing"
        );
        assert_eq!(
            fired_offsets(&fired),
            rises,
            "{will}: and nothing pages for either"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(unix)]
#[test]
fn a_need_standing_since_before_the_warm_window_pages_nothing_again_after_a_restart() {
    let dir = scratch("need-window");
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let broker = crate::test_broker(base.join("w.log")).unwrap();
    let sock = base.join("w.sock");
    let mut server = broker.serve(&sock).unwrap();
    let (mut conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
    let mut seq = 0;
    let mut publish = |conn: &mut Conn, subject: &str, body: &[u8]| -> u64 {
        seq += 1;
        conn.publish(7, seq, subject, body).unwrap().0
    };
    let mut s = Needs::open();
    let cmd = s.cmd_open();
    s.cmd_close(cmd, aterm_slog::RunOut::Fail);
    let mut rose = None;
    for b in s.bodies() {
        let off = publish(&mut conn, NEED_FACE, b.as_bytes());
        if b.contains("out=fail") {
            rose = Some(off);
        }
    }
    let rose = rose.expect("the failed run's close");
    for _ in 0..crate::notify_need::WARM_WINDOW + 10 {
        publish(&mut conn, "/f/f1/pub/n-a/s-z/filler", b"v=1 t=1");
    }
    let state = base.join("state");
    let fired = base.join("fired");
    let run = || {
        let mut c = cfg(&state.to_string_lossy());
        c.broker = sock.to_string_lossy().into_owned();
        c.since = Since::Start;
        c.limit = 100;
        c.on = vec![Sel::Need(None)];
        c.exec = format!(
            "printf '%s\\n' \"$ATERM_NOTIFY_OFFSET\" >> {}",
            fired.display()
        );
        let mut n = Notifier::new(c).expect("open");
        n.catch_up().expect("catch up");
        fired_offsets(&fired)
    };
    assert_eq!(run(), [rose], "the rise pages once");

    s.quiet();
    let partial = s.bodies();
    assert!(
        partial.iter().all(|b| !b.contains(" op=snap ")),
        "PRECONDITION: a record the restarted watch folds from no start"
    );
    for b in partial {
        publish(&mut conn, NEED_FACE, b.as_bytes());
    }
    assert_eq!(
        run(),
        [rose],
        "a partial fold past the warm window clears nothing"
    );
    for _ in 0..=aterm_slog::SNAP_EVERY {
        s.quiet();
    }
    let reseeded = s.bodies();
    assert!(
        reseeded.iter().any(|b| b.contains(" op=snap ")),
        "PRECONDITION: a snapshot reseeds the standing failure"
    );
    for b in reseeded {
        publish(&mut conn, NEED_FACE, b.as_bytes());
    }
    assert_eq!(
        run(),
        [rose],
        "the snapshot's need was standing: nothing pages again"
    );
    let cmd = s.cmd_open();
    s.cmd_close(cmd, aterm_slog::RunOut::Ok);
    s.question();
    let mut asked = None;
    for b in s.bodies() {
        let off = publish(&mut conn, NEED_FACE, b.as_bytes());
        if b.contains("k=question") {
            asked = Some(off);
        }
    }
    assert_eq!(
        run(),
        [rose, asked.expect("the question's open")],
        "the seeded fold judges again: the next rise pages"
    );
    drop(conn);
    server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_sessions_exit_pages_once_whichever_of_its_close_and_its_row_comes_first() {
    let presence = "/f/f1/pub/n-a/tzzzzzzzzzx/presence";
    let exited: &[u8] = b"v=1 t=2 inc=1 state=exited";
    let mut s = Needs::open();
    let cmd = s.cmd_open();
    s.cmd_close(cmd, aterm_slog::RunOut::Fail);
    let failing = s.bodies();
    s.exit();
    let closing = s.bodies();
    assert!(
        closing.iter().any(|b| b.contains(" op=close node=- ")),
        "PRECONDITION: the session's close: {closing:?}"
    );
    for (tag, sel) in [
        ("exited", Sel::Need(Some(vec!["exited"]))),
        ("any", Sel::Need(None)),
    ] {
        for close_first in [true, false] {
            let dir = scratch(&format!("need-exit-{tag}-{close_first}"));
            let (mut n, fired) = appending(&dir, 100, "");
            n.cfg.on = vec![sel.clone()];
            let mut off = 0;
            let mut feed = |n: &mut Notifier, subject: &str, body: &[u8]| {
                off += 1;
                n.on_record(&sel, off, subject, body).expect("on_record");
                off
            };
            let mut want = Vec::new();
            for b in &failing {
                let at = feed(&mut n, NEED_FACE, b.as_bytes());
                if b.contains("out=fail") && tag == "any" {
                    want.push(at);
                }
            }
            if !close_first {
                feed(&mut n, presence, exited);
            }
            for b in &closing {
                let at = feed(&mut n, NEED_FACE, b.as_bytes());
                if b.contains(" op=close node=- ") {
                    want.push(at);
                }
            }
            if close_first {
                feed(&mut n, presence, exited);
            }
            feed(
                &mut n,
                "/f/f1/pub/n-a/node/presence",
                b"v=1 t=3 state=gone inc=1",
            );
            assert_eq!(
                fired_offsets(&fired),
                want,
                "{tag}, close first {close_first}: the exit pages once, at the close"
            );
            assert_eq!(
                standing_words(&n, NEED_FACE),
                ["exited"],
                "{tag}, close first {close_first}: and stands"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// The bodies of a relaunch's new shell, whose restore names `prev`: `open - restore
/// prev=<prev> lineage=<prev>` (aterm-gui's `note_restored` before its open).
fn restore_of(prev: &str) -> Vec<String> {
    let mut log = aterm_slog::Log::new(2);
    let f = aterm_slog::Fields::new()
        .id(aterm_slog::Key::Prev, prev)
        .and_then(|f| f.id(aterm_slog::Key::Lineage, prev))
        .expect("the restore's fields");
    log.open_session(
        aterm_slog::Kind::Restore,
        aterm_slog::Src::Aterm,
        aterm_slog::Conf::Exact,
        aterm_slog::Actor::Nobody,
        f,
        aterm_slog::Stamp::new(1_760_000_100_000, 0),
    )
    .expect("opens");
    let bodies = mirrored(&mut log);
    assert!(
        bodies
            .iter()
            .any(|b| b.contains(" k=restore ") && b.contains(&format!(" prev={prev}"))),
        "PRECONDITION: the restore crosses the bus naming its predecessor: {bodies:?}"
    );
    bodies
}

/// A session that pages a failure and then waits in an agent turn: its bodies.
fn failed_then_waiting() -> Vec<String> {
    let mut s = Needs::open();
    let cmd = s.cmd_open();
    s.cmd_close(cmd, aterm_slog::RunOut::Fail);
    s.turn();
    s.bodies()
}

/// A DEAD SESSION'S EXIT PAGES ONCE, AND ONLY AT A SUCCESSOR'S RESTORE (fix unit E9, finding
/// E2E-5; the orchestrator's 2026-10-27 ruling). Two sessions each page a failure and then
/// wait in an agent turn; their node's bridge dies (its will) and the node relaunches. A
/// successor's restore names tzzzzzzzzzx: the watch derives tzzzzzzzzzx's close and pages `exited`, detail
/// `gone`, for tzzzzzzzzzx, once, at the restore, under bare `--on need` and under `need:exited`.
/// Nothing else pages: tzzzzzzzzzx's own `exited` row after it, the restore delivered again, a
/// record of tzzzzzzzzzx delivered again, and tzzzzzzzzzy, whose row a later incarnation retired with no
/// successor naming it (the brief derives its close; its row alone pages nothing, and its
/// pairs clear, as G2's rule says), nor a record of tzzzzzzzzzy delivered after that row. NEGATIVE
/// CONTROLS, run: a watch that derives nothing (the restore pages nothing), one that judges
/// the restore as the successor's record only (the exit pages late, at tzzzzzzzzzx's next record),
/// one that derives on a retired row too (tzzzzzzzzzy pages at its next record), and one that pages
/// the derived exit without its `gone`.
#[test]
fn a_successors_restore_pages_a_dead_sessions_exit_once_and_its_row_alone_pages_nothing() {
    let face = |sid: &str| format!("/f/f1/pub/n-a/{sid}/ev");
    let row = |sid: &str| format!("/f/f1/pub/n-a/{sid}/presence");
    let waiting = failed_then_waiting();
    let restore = restore_of("tzzzzzzzzzx");
    for (tag, sel) in [
        ("exited", Sel::Need(Some(vec!["exited"]))),
        ("any", Sel::Need(None)),
    ] {
        let dir = scratch(&format!("need-restore-{tag}"));
        let said = Path::new(&dir).join("said");
        let (mut n, fired) = appending(
            &dir,
            100,
            &format!(
                "printf '%s %s %s\\n' \"$ATERM_NOTIFY_NEED\" \"$ATERM_NOTIFY_DETAIL\" \
                 \"$ATERM_NOTIFY_SID\" >> {}; ",
                said.display()
            ),
        );
        n.cfg.on = vec![sel.clone()];
        let mut off = 0;
        let mut feed = |n: &mut Notifier, subject: &str, body: &[u8]| {
            off += 1;
            n.on_record(&sel, off, subject, body).expect("on_record");
            off
        };
        let mut want = Vec::new();
        for sid in ["tzzzzzzzzzx", "tzzzzzzzzzy"] {
            feed(&mut n, &row(sid), b"v=1 t=1 inc=1 state=live");
            for b in &waiting {
                let at = feed(&mut n, &face(sid), b.as_bytes());
                if b.contains("out=fail") && tag == "any" {
                    want.push(at);
                }
            }
        }
        feed(
            &mut n,
            &row("node"),
            b"v=1 t=2 state=away inc=1 fabric=disconnected",
        );
        feed(&mut n, &row("node"), b"v=1 t=3 state=live inc=2");
        for b in &restore {
            let at = feed(&mut n, &face("tzzzzzzzzzz"), b.as_bytes());
            if b.contains(" k=restore ") {
                want.push(at);
            }
        }
        assert_eq!(
            fired_offsets(&fired),
            want,
            "{tag}: the dead session's exit pages at its successor's restore"
        );
        feed(&mut n, &row("tzzzzzzzzzx"), b"v=1 t=4 inc=2 state=exited");
        feed(&mut n, &row("tzzzzzzzzzy"), b"v=1 t=4 inc=2 state=exited");
        for b in &restore {
            feed(&mut n, &face("tzzzzzzzzzz"), b.as_bytes());
        }
        for sid in ["tzzzzzzzzzx", "tzzzzzzzzzy"] {
            feed(&mut n, &face(sid), waiting[0].as_bytes());
        }
        assert_eq!(fired_offsets(&fired), want, "{tag}: and nothing else pages");
        let said = std::fs::read_to_string(&said).unwrap_or_default();
        assert_eq!(
            said.lines()
                .filter(|l| l.starts_with("exited "))
                .collect::<Vec<_>>(),
            ["exited gone tzzzzzzzzzx"],
            "{tag}: {said}"
        );
        assert_eq!(
            standing_words(&n, &face("tzzzzzzzzzx")),
            ["exited"],
            "{tag}"
        );
        assert_eq!(
            standing_words(&n, &face("tzzzzzzzzzy")),
            Vec::<&str>::new(),
            "{tag}: a retired row alone clears its session's pairs"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A QUIT'S RELAUNCH AND A REOPENED TAB PAGE NO EXIT FOR THE SESSION THEIR RESTORE NAMES (fix
/// unit E9, the lander's review). aterm writes `restore prev=<sid>` whenever a new shell fills
/// a pane: after a clean quit's relaunch too, and when a closed tab is reopened. On n-a, tzzzzzzzzzq
/// pages a failure and waits in a turn; its aterm quits (the bridge withdraws it, `exited`
/// under `inc=1`, the incarnation that hosted it; its close stays in the journal), its node's
/// will fires as the bridge goes, the node relaunches under `inc=2`, and tzzzzzzzzzz's restore names
/// tzzzzzzzzzq: nothing pages for tzzzzzzzzzq's exit (its row alone says it exited, G2's rule), and its pairs
/// clear. On n-b, tzzzzzzzzzt waits in a turn, its tab closes and is reopened under the same
/// incarnation (`inc=1`): tzzzzzzzzz0's restore names tzzzzzzzzzt before tzzzzzzzzzt's own close reaches the bus,
/// and tzzzzzzzzzt's `exited` pages once, at that close. Under bare `--on need` and `need:exited`.
/// NEGATIVE CONTROLS, run: a restore that counts whatever withdrew the session pages
/// `exited gone tzzzzzzzzzq` at tzzzzzzzzzz's restore; one that counts while the session's own incarnation
/// lives pages tzzzzzzzzzt's exit twice, at tzzzzzzzzz0's restore and at tzzzzzzzzzt's close.
#[test]
fn a_quits_relaunch_and_a_reopened_tab_page_no_exit_for_the_session_their_restore_names() {
    let face = |node: &str, sid: &str| format!("/f/f1/pub/{node}/{sid}/ev");
    let row = |node: &str, sid: &str| format!("/f/f1/pub/{node}/{sid}/presence");
    let waiting = failed_then_waiting();
    let mut tab = Needs::open();
    tab.turn();
    let tab_open = tab.bodies();
    tab.exit();
    let tab_close = tab.bodies();
    for (tag, sel) in [
        ("exited", Sel::Need(Some(vec!["exited"]))),
        ("any", Sel::Need(None)),
    ] {
        let dir = scratch(&format!("need-restore-quit-{tag}"));
        let said = Path::new(&dir).join("said");
        let (mut n, fired) = appending(
            &dir,
            100,
            &format!(
                "printf '%s %s %s\\n' \"$ATERM_NOTIFY_NEED\" \"$ATERM_NOTIFY_DETAIL\" \
                 \"$ATERM_NOTIFY_SID\" >> {}; ",
                said.display()
            ),
        );
        n.cfg.on = vec![sel.clone()];
        let mut off = 0;
        let mut feed = |n: &mut Notifier, subject: &str, body: &[u8]| {
            off += 1;
            n.on_record(&sel, off, subject, body).expect("on_record");
            off
        };
        let mut want = Vec::new();
        feed(&mut n, &row("n-a", "node"), b"v=1 t=1 state=live inc=1");
        feed(
            &mut n,
            &row("n-a", "tzzzzzzzzzq"),
            b"v=1 t=1 inc=1 state=live",
        );
        for b in &waiting {
            let at = feed(&mut n, &face("n-a", "tzzzzzzzzzq"), b.as_bytes());
            if b.contains("out=fail") && tag == "any" {
                want.push(at);
            }
        }
        feed(
            &mut n,
            &row("n-a", "tzzzzzzzzzq"),
            b"v=1 t=2 inc=1 state=exited",
        );
        feed(
            &mut n,
            &row("n-a", "node"),
            b"v=1 t=2 state=away inc=1 fabric=disconnected",
        );
        feed(&mut n, &row("n-a", "node"), b"v=1 t=3 state=live inc=2");
        for b in restore_of("tzzzzzzzzzq") {
            feed(&mut n, &face("n-a", "tzzzzzzzzzz"), b.as_bytes());
        }
        for b in &waiting {
            feed(&mut n, &face("n-a", "tzzzzzzzzzq"), b.as_bytes());
        }
        assert_eq!(
            fired_offsets(&fired),
            want,
            "{tag}: a quit's relaunch pages no exit for the session it withdrew"
        );
        assert_eq!(
            standing_words(&n, &face("n-a", "tzzzzzzzzzq")),
            Vec::<&str>::new(),
            "{tag}: its row alone clears its pairs"
        );

        feed(&mut n, &row("n-b", "node"), b"v=1 t=1 state=live inc=1");
        feed(
            &mut n,
            &row("n-b", "tzzzzzzzzzt"),
            b"v=1 t=1 inc=1 state=live",
        );
        for b in &tab_open {
            feed(&mut n, &face("n-b", "tzzzzzzzzzt"), b.as_bytes());
        }
        for b in restore_of("tzzzzzzzzzt") {
            feed(&mut n, &face("n-b", "tzzzzzzzzz0"), b.as_bytes());
        }
        for b in &tab_close {
            let at = feed(&mut n, &face("n-b", "tzzzzzzzzzt"), b.as_bytes());
            if b.contains(" op=close node=- ") {
                want.push(at);
            }
        }
        feed(
            &mut n,
            &row("n-b", "tzzzzzzzzzt"),
            b"v=1 t=2 inc=1 state=exited",
        );
        assert_eq!(
            fired_offsets(&fired),
            want,
            "{tag}: a reopened tab's session pages its exit once, at its own close"
        );
        let said = std::fs::read_to_string(&said).unwrap_or_default();
        assert_eq!(
            said.lines()
                .filter(|l| l.starts_with("exited "))
                .collect::<Vec<_>>(),
            ["exited  tzzzzzzzzzt"],
            "{tag}: {said}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A CLEAN CLOSE NEVER DERIVES (fix unit E9). A session whose close reached the bus pages its
/// `exited` once, at its close; a successor's restore naming it afterwards pages nothing more
/// and leaves that one pair standing. NEGATIVE CONTROL, run: a watch that derives over a fold
/// that holds its close pages a second `exited` at the restore.
#[test]
fn a_restore_naming_a_session_whose_close_reached_the_bus_pages_nothing_more() {
    let mut s = Needs::open();
    s.turn();
    s.exit();
    let closing = s.bodies();
    let restore = restore_of("tzzzzzzzzzx");
    for (tag, sel) in [
        ("exited", Sel::Need(Some(vec!["exited"]))),
        ("any", Sel::Need(None)),
    ] {
        let dir = scratch(&format!("need-restore-closed-{tag}"));
        let (mut n, fired) = appending(&dir, 100, "");
        n.cfg.on = vec![sel.clone()];
        let mut off = 0;
        let mut feed = |n: &mut Notifier, subject: &str, body: &[u8]| {
            off += 1;
            n.on_record(&sel, off, subject, body).expect("on_record");
            off
        };
        let mut want = Vec::new();
        for b in &closing {
            let at = feed(&mut n, NEED_FACE, b.as_bytes());
            if b.contains(" op=close node=- ") {
                want.push(at);
            }
        }
        assert_eq!(
            want.len(),
            1,
            "PRECONDITION: one session close: {closing:?}"
        );
        for b in &restore {
            feed(&mut n, "/f/f1/pub/n-a/tzzzzzzzzzz/ev", b.as_bytes());
        }
        assert_eq!(
            fired_offsets(&fired),
            want,
            "{tag}: the exit pages once, at the close"
        );
        assert_eq!(standing_words(&n, NEED_FACE), ["exited"], "{tag}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A CLEAN QUIT'S CLOSE AND ITS RELAUNCH PAGE NOTHING; A CRASH PAGES ITS EXIT ONCE (fix unit
/// Q1). On n-a, tzzzzzzzzzq pages a failure and waits in a turn, and its aterm quits cleanly: its
/// close `out=app-quit` reaches the bus before the bridge's `bye quit`, with the bridge's
/// withdrawal after it (`exited` under `inc=1`) or with none (a withdrawal that never
/// landed); the node's will fires as the bridge goes, the node relaunches under `inc=2`, and
/// tzzzzzzzzzz's restore names tzzzzzzzzzq: nothing pages for tzzzzzzzzzq but its failure, and none of its pairs
/// stands. On n-c, tzzzzzzzzzc pages the same failure and waits, and its aterm crashes (no close, no
/// withdrawal); the node relaunches, and tzzzzzzzzzw's restore names tzzzzzzzzzc: `exited gone tzzzzzzzzzc` pages
/// once. Under bare `--on need` and `need:exited`. NEGATIVE CONTROLS, run: a watch that pages
/// a quit's close as any other close pages `exited` at tzzzzzzzzzq's close; one that pages no
/// `exited` at all pages nothing for tzzzzzzzzzc.
#[test]
fn a_clean_quits_close_and_its_relaunch_page_nothing_and_a_crash_pages_its_exit_once() {
    let face = |node: &str, sid: &str| format!("/f/f1/pub/{node}/{sid}/ev");
    let row = |node: &str, sid: &str| format!("/f/f1/pub/{node}/{sid}/presence");
    let mut quitting = Needs::open();
    let cmd = quitting.cmd_open();
    quitting.cmd_close(cmd, aterm_slog::RunOut::Fail);
    quitting.turn();
    let waiting = quitting.bodies();
    quitting.quit();
    let quit = quitting.bodies();
    assert!(
        quit.iter()
            .any(|b| b.contains(" op=close node=- ") && b.contains(" out=app-quit")),
        "PRECONDITION: the quit's close crosses the bus: {quit:?}"
    );
    for withdrawn in [true, false] {
        for (tag, sel) in [
            ("exited", Sel::Need(Some(vec!["exited"]))),
            ("any", Sel::Need(None)),
        ] {
            let dir = scratch(&format!("need-quit-{withdrawn}-{tag}"));
            let said = Path::new(&dir).join("said");
            let (mut n, fired) = appending(
                &dir,
                100,
                &format!(
                    "printf '%s %s %s\\n' \"$ATERM_NOTIFY_NEED\" \"$ATERM_NOTIFY_DETAIL\" \
                     \"$ATERM_NOTIFY_SID\" >> {}; ",
                    said.display()
                ),
            );
            n.cfg.on = vec![sel.clone()];
            let mut off = 0;
            let mut feed = |n: &mut Notifier, subject: &str, body: &[u8]| {
                off += 1;
                n.on_record(&sel, off, subject, body).expect("on_record");
                off
            };
            let mut want = Vec::new();
            for (node, sid) in [("n-a", "tzzzzzzzzzq"), ("n-c", "tzzzzzzzzzc")] {
                feed(&mut n, &row(node, "node"), b"v=1 t=1 state=live inc=1");
                feed(&mut n, &row(node, sid), b"v=1 t=1 inc=1 state=live");
                for b in &waiting {
                    let at = feed(&mut n, &face(node, sid), b.as_bytes());
                    if b.contains("out=fail") && tag == "any" {
                        want.push(at);
                    }
                }
            }
            for b in &quit {
                feed(&mut n, &face("n-a", "tzzzzzzzzzq"), b.as_bytes());
            }
            if withdrawn {
                feed(
                    &mut n,
                    &row("n-a", "tzzzzzzzzzq"),
                    b"v=1 t=2 inc=1 state=exited",
                );
            }
            for node in ["n-a", "n-c"] {
                feed(
                    &mut n,
                    &row(node, "node"),
                    b"v=1 t=2 state=away inc=1 fabric=disconnected",
                );
                feed(&mut n, &row(node, "node"), b"v=1 t=3 state=live inc=2");
            }
            for b in restore_of("tzzzzzzzzzq") {
                feed(&mut n, &face("n-a", "tzzzzzzzzzz"), b.as_bytes());
            }
            assert_eq!(
                fired_offsets(&fired),
                want,
                "{tag}, withdrawn {withdrawn}: a clean quit's close and its relaunch page \
                 nothing"
            );
            assert_eq!(
                standing_words(&n, &face("n-a", "tzzzzzzzzzq")),
                Vec::<&str>::new(),
                "{tag}, withdrawn {withdrawn}: the quit's close leaves no pair standing"
            );
            for b in restore_of("tzzzzzzzzzc") {
                let at = feed(&mut n, &face("n-c", "tzzzzzzzzzw"), b.as_bytes());
                if b.contains(" k=restore ") {
                    want.push(at);
                }
            }
            assert_eq!(
                fired_offsets(&fired),
                want,
                "{tag}, withdrawn {withdrawn}: the crash pages its exit once, at the restore"
            );
            let said = std::fs::read_to_string(&said).unwrap_or_default();
            assert_eq!(
                said.lines()
                    .filter(|l| l.starts_with("exited "))
                    .collect::<Vec<_>>(),
                ["exited gone tzzzzzzzzzc"],
                "{tag}, withdrawn {withdrawn}: {said}"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// A derived exit keeps one terminal receipt when a later foreign body moves the
/// derived sequence, an older body arrives after it, or the real close finally arrives.
/// These are separate fresh stores so the late real close is also tested without the
/// foreign body first. Every catch-up is a real notifier `--once` over the private broker.
#[cfg(unix)]
#[test]
fn a_derived_exit_stands_through_later_foreign_records_and_its_real_close() {
    for foreign_first in [false, true] {
        let dir = scratch(&format!("late-{foreign_first}"));
        let base = PathBuf::from(&dir);
        std::fs::create_dir_all(&base).expect("scratch");
        let broker = crate::test_broker(base.join("b.log")).unwrap();
        let mut bus = NeedBus::open(&base, &broker);
        let sock = bus.sock.clone();
        let mut session = Needs::open();
        session.turn();
        let waiting = session.bodies();
        let held: Vec<_> = waiting
            .iter()
            .filter_map(|body| aterm_slog::decode_bus(body).ok())
            .collect();
        let head = held.iter().map(|b| b.rec.seq).max().expect("a head");
        let ms = held
            .iter()
            .filter_map(|b| b.ms)
            .max()
            .expect("a mirror head");
        bus.row("/f/f1/pub/n-a/node/presence", "v=1 t=1 state=live inc=1");
        bus.row(
            "/f/f1/pub/n-a/tzzzzzzzzzx/presence",
            "v=1 t=1 inc=1 state=live",
        );
        bus.publish(NEED_FACE, waiting);
        bus.row(
            "/f/f1/pub/n-a/node/presence",
            "v=1 t=2 state=away inc=1 fabric=disconnected",
        );
        bus.row("/f/f1/pub/n-a/node/presence", "v=1 t=3 state=live inc=2");
        let restored = offset_of(
            &bus.publish("/f/f1/pub/n-a/tzzzzzzzzzz/ev", restore_of("tzzzzzzzzzx")),
            &[" k=restore "],
        );
        let state = base.join("follow");
        let sel = Sel::Need(Some(vec!["exited"]));
        let paged = need_once(&sock, &state, sel.clone(), Since::Start);
        assert_eq!(paged, [format!("{restored} exited")]);
        let saved = || {
            let (config, _) = need_cfg(&sock, &state, sel.clone(), Since::Start);
            Notifier::new(config)
                .expect("inspect store")
                .pairs_of(NEED_FACE)
        };
        let original = saved();
        let [Pair {
            word: "exited",
            raised:
                crate::notify_need::Raised::At {
                    ep: 1,
                    seq: derived,
                },
        }] = original.as_slice()
        else {
            panic!("the derived raise keeps its observed sequence: {original:?}");
        };
        if foreign_first {
            for (seq_delta, ms_delta) in [(32, 10), (16, 9)] {
                let body = format!(
                    "v=2 t=1760000000009 ep=1 seq={} ms={} op=mark node=- k=zzznew src=mark conf=claim",
                    head + seq_delta,
                    ms + ms_delta
                );
                let record = aterm_slog::decode_bus(&body).expect("valid newer-writer body");
                assert!(
                    record.rec.seq > *derived,
                    "a real change to the offered head"
                );
                assert_eq!(record.rec.k, aterm_slog::Kind::Unknown);
                bus.publish(NEED_FACE, vec![body]);
                assert_eq!(
                    need_once(&sock, &state, sel.clone(), Since::Start),
                    paged,
                    "foreign first {foreign_first}: later and reordered bodies page no exit"
                );
                assert_eq!(saved(), original, "the stored sequence is not rewritten");
            }
        }
        session.exit();
        let closing = session.bodies();
        let real = closing
            .iter()
            .filter_map(|body| aterm_slog::decode_bus(body).ok())
            .find(|b| b.rec.op == aterm_slog::Op::Close && b.rec.node == aterm_slog::Node::SESSION)
            .expect("the actual session close");
        assert_ne!(
            real.rec.seq, *derived,
            "the late close has another sequence"
        );
        bus.publish(NEED_FACE, closing);
        for _ in 0..2 {
            assert_eq!(
                need_once(&sock, &state, sel.clone(), Since::Start),
                paged,
                "foreign first {foreign_first}: the real close and restart keep one receipt"
            );
        }
        assert_eq!(
            saved(),
            original,
            "the original token retains its provenance"
        );
        assert!(need_once(&sock, &base.join("late"), sel, Since::Head).is_empty());
        drop(bus.conn);
        bus.server.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Only the session's terminal exit is one identity per epoch. Two failed commands
/// while failure is already standing still page twice, and another epoch's session close
/// pages again even when its start was not retained and the old exit still stands.
#[cfg(unix)]
#[test]
fn renewable_failures_and_another_epochs_exit_keep_their_own_notification_identity() {
    let dir = scratch("need-terminal-positive");
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let broker = crate::test_broker(base.join("b.log")).unwrap();
    let mut bus = NeedBus::open(&base, &broker);
    let sock = bus.sock.clone();
    let state = base.join("follow");
    let mut session = Needs::open();
    let mut want = Vec::new();
    for _ in 0..2 {
        let command = session.cmd_open();
        session.cmd_close(command, aterm_slog::RunOut::Fail);
        let offset = offset_of(&bus.publish(NEED_FACE, session.bodies()), &["out=fail"]);
        want.push(format!("{offset} failure"));
    }
    session.exit();
    let exit = offset_of(
        &bus.publish(NEED_FACE, session.bodies()),
        &[" op=close node=- "],
    );
    want.push(format!("{exit} exited"));
    assert_eq!(
        need_once(&sock, &state, Sel::Need(None), Since::Start),
        want
    );

    let mut next = Needs::in_space(2);
    next.exit();
    let closing: Vec<_> = next
        .bodies()
        .into_iter()
        .filter(|body| body.contains(" op=close node=- "))
        .collect();
    assert_eq!(closing.len(), 1, "a real close with its start absent");
    assert_eq!(aterm_slog::decode_bus(&closing[0]).expect("close").ep, 2);
    let exit = bus.publish(NEED_FACE, closing)[0].0;
    want.push(format!("{exit} exited"));
    assert_eq!(
        need_once(&sock, &state, Sel::Need(None), Since::Start),
        want
    );
    assert_eq!(
        need_once(&sock, &state, Sel::Need(None), Since::Start),
        want
    );
    drop(bus.conn);
    bus.server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A DERIVED EXIT PAGES NOTHING AGAIN, AFTER A RESTART OR A LATE START (fix unit E9). A
/// notifier pages a dead session's failure and then, its aterm dead (its row live under
/// `inc=1`, its node's will, the node live under `inc=2`), its `exited` at its successor's
/// restore.
/// Its restart, which keeps the pair that page left standing, and a notifier started at the
/// head after the restore, whose warm-up folds both below its cursor and stands the derived
/// `exited` ([`NeedWatch::standing`]), page nothing at a record of the dead session
/// delivered again past their cursor, nor at its `exited` row. NEGATIVE CONTROL, run: a
/// warm-up that does not stand a derived exit pages it at the record delivered again, in
/// the run started at the head.
#[cfg(unix)]
#[test]
fn a_dead_sessions_derived_exit_pages_nothing_again_after_a_restart_or_a_late_start() {
    let dir = scratch("need-restore-restart");
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let broker = crate::test_broker(base.join("b.log")).unwrap();
    let mut bus = NeedBus::open(&base, &broker);
    let sock = bus.sock.clone();
    let waiting = failed_then_waiting();
    bus.row("/f/f1/pub/n-a/node/presence", "v=1 t=1 state=live inc=1");
    bus.row(
        "/f/f1/pub/n-a/tzzzzzzzzzx/presence",
        "v=1 t=1 inc=1 state=live",
    );
    let failed = offset_of(&bus.publish(NEED_FACE, waiting.clone()), &["out=fail"]);
    bus.row(
        "/f/f1/pub/n-a/node/presence",
        "v=1 t=2 state=away inc=1 fabric=disconnected",
    );
    bus.row("/f/f1/pub/n-a/node/presence", "v=1 t=3 state=live inc=2");
    let restored = offset_of(
        &bus.publish("/f/f1/pub/n-a/tzzzzzzzzzz/ev", restore_of("tzzzzzzzzzx")),
        &[" k=restore "],
    );
    let (follow, late) = (base.join("follow"), base.join("late"));
    let paged = need_once(&sock, &follow, Sel::Need(None), Since::Start);
    assert_eq!(paged.len(), 2, "the failure, then the exit: {paged:?}");
    assert!(
        paged[0].starts_with(&format!("{failed} ")),
        "the failure first: {paged:?}"
    );
    assert_eq!(
        paged[1],
        format!("{restored} exited"),
        "the exit at the restore"
    );
    let none: Vec<String> = Vec::new();
    assert_eq!(
        need_once(&sock, &late, Sel::Need(None), Since::Head),
        none,
        "a notifier started at the head after the restore pages nothing at its start"
    );
    bus.publish(NEED_FACE, vec![waiting[0].clone()]);
    bus.publish(
        "/f/f1/pub/n-a/tzzzzzzzzzx/presence",
        vec!["v=1 t=4 inc=2 state=exited".to_string()],
    );
    assert_eq!(
        need_once(&sock, &follow, Sel::Need(None), Since::Start),
        paged,
        "a restart pages the derived exit nothing again"
    );
    assert_eq!(
        need_once(&sock, &late, Sel::Need(None), Since::Head),
        none,
        "nor does the late start, whose warm-up stood it"
    );
    drop(bus.conn);
    bus.server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_record_held_for_durability_is_judged_before_a_reconnects_warm_up_folds_it() {
    let log = ReadLog {
        id: Some(astream_broker::LogIdentity {
            id: [7; 16],
            generation: 0,
        }),
        lineage: None,
        proved: Steps::default(),
        ordered: true,
        carried: 0,
    };
    let mut s = Needs::open();
    let cmd = s.cmd_open();
    s.cmd_close(cmd, aterm_slog::RunOut::Fail);
    let cmd = s.cmd_open();
    s.cmd_close(cmd, aterm_slog::RunOut::Ok);
    let records: Vec<(u64, String)> = (1..).zip(s.bodies()).collect();
    let rose = records
        .iter()
        .find(|(_, b)| b.contains("out=fail"))
        .map(|(off, _)| *off)
        .expect("the failed run's close");
    assert!(
        records
            .iter()
            .any(|(off, b)| *off > rose && b.contains("out=ok")),
        "PRECONDITION: the run that lifts it comes after"
    );
    let dir = scratch("need-held");
    let (mut n, fired) = appending(&dir, 100, "");
    n.cfg.on = vec![Sel::Need(None)];
    let mut notices = vec![Notice::Log(0, log.clone())];
    for (off, b) in &records {
        notices.push(Notice::Record(
            0,
            *off,
            NEED_FACE.into(),
            b.clone().into_bytes(),
        ));
    }
    notices.push(Notice::Log(0, log));
    for (off, b) in &records {
        notices.push(Notice::Warm(
            0,
            *off,
            NEED_FACE.into(),
            b.clone().into_bytes(),
        ));
    }
    for notice in notices {
        n.take(notice).expect("take");
    }
    assert!(fired_offsets(&fired).is_empty(), "held, not paged");
    n.take(Notice::Durable(0, u64::MAX)).expect("durable");
    assert_eq!(
        fired_offsets(&fired),
        [rose],
        "the held failure is judged by the fold as it stood at its own record"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Each line a test's command wrote to `fired`.
fn fired_lines(fired: &Path) -> Vec<String> {
    std::fs::read_to_string(fired)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// A need test's config over the broker at `sock`, its state `state`: `on` from `since`, its
/// command writing `<offset> <need>` to `<state>.fired`.
fn need_cfg(sock: &Path, state: &Path, on: Sel, since: Since) -> (Config, PathBuf) {
    let fired = state.with_extension("fired");
    let mut c = cfg(&state.to_string_lossy());
    c.broker = sock.to_string_lossy().into_owned();
    c.since = since;
    c.limit = 100;
    c.on = vec![on];
    c.exec = format!(
        "printf '%s %s\\n' \"$ATERM_NOTIFY_OFFSET\" \"$ATERM_NOTIFY_NEED\" >> {}",
        fired.display()
    );
    (c, fired)
}

/// One `--once` run of [`need_cfg`]: every line its command has written on that state.
fn need_once(sock: &Path, state: &Path, on: Sel, since: Since) -> Vec<String> {
    let (c, fired) = need_cfg(sock, state, on, since);
    Notifier::new(c)
        .expect("open")
        .catch_up()
        .expect("catch up");
    fired_lines(&fired)
}

/// A broker in `dir` for a need test, and a publisher on it: each body to `subject`, its
/// offset with it.
struct NeedBus {
    server: astream_broker::BrokerHandle,
    conn: Conn,
    seq: u64,
    sock: PathBuf,
}

impl NeedBus {
    fn open(base: &Path, broker: &astream_broker::Broker) -> Self {
        let sock = base.join("b.sock");
        let server = broker.serve(&sock).unwrap();
        let (conn, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
        Self {
            server,
            conn,
            seq: 0,
            sock,
        }
    }

    fn publish(&mut self, subject: &str, bodies: Vec<String>) -> Vec<(u64, String)> {
        bodies
            .into_iter()
            .map(|b| {
                self.seq += 1;
                let off = self
                    .conn
                    .publish(7, self.seq, subject, b.as_bytes())
                    .unwrap()
                    .0;
                (off, b)
            })
            .collect()
    }

    fn row(&mut self, subject: &str, body: &str) -> u64 {
        self.publish(subject, vec![body.to_string()])[0].0
    }
}

/// The offset of the first of `page`'s bodies that holds every one of `words`.
fn offset_of(page: &[(u64, String)], words: &[&str]) -> u64 {
    page.iter()
        .find(|(_, b)| words.iter().all(|w| b.contains(w)))
        .map(|(off, _)| *off)
        .unwrap_or_else(|| panic!("no body holds {words:?}: {page:?}"))
}

#[cfg(unix)]
#[test]
fn a_notifier_started_over_a_standing_need_pages_only_what_rises_past_its_cursor() {
    let dir = scratch("need-head");
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let broker = crate::test_broker(base.join("b.log")).unwrap();
    let mut bus = NeedBus::open(&base, &broker);
    let sock = bus.sock.clone();
    let mut s = Needs::open();
    let cmd = s.cmd_open();
    s.cmd_close(cmd, aterm_slog::RunOut::Fail);
    bus.publish(NEED_FACE, s.bodies());
    let once = base.join("once");
    let none: Vec<String> = Vec::new();
    assert_eq!(
        need_once(&sock, &once, Sel::Need(None), Since::Head),
        none,
        "a fresh notifier at the head pages nothing at its start"
    );

    let (mut c, fired) = need_cfg(&sock, &base.join("follow"), Sel::Need(None), Since::Head);
    c.once = false;
    let mut n = Notifier::new(c.clone()).expect("open");
    let (tx, rx) = notice_handoff();
    let follower = {
        let hold = std::sync::Arc::clone(&n.hold);
        std::thread::spawn(move || reader(0, &Sel::Need(None), &c, &hold, &tx))
    };
    pump(&mut n, &rx, "the reader's subscription", |_| {
        broker.stats().parked_tails == 1
    });

    s.mail();
    let snapped = bus.publish(NEED_FACE, s.bodies());
    assert!(
        snapped.iter().any(|(_, b)| b.contains(" op=snap ")),
        "PRECONDITION: the mail count's snapshot reaches the bus: {snapped:?}"
    );
    s.cmd_open();
    let opened = bus.publish(NEED_FACE, s.bodies());
    assert!(
        !opened.is_empty(),
        "PRECONDITION: a run opens past the cursor"
    );
    s.question();
    let asked = offset_of(&bus.publish(NEED_FACE, s.bodies()), &["k=question"]);
    pump(&mut n, &rx, "a page", |_| !fired_lines(&fired).is_empty());
    let question = [format!("{asked} question")];
    assert_eq!(
        fired_lines(&fired),
        question,
        "the follower pages the question alone: the failure stood before it started"
    );
    assert_eq!(
        need_once(&sock, &once, Sel::Need(None), Since::Head),
        question,
        "so does the --once run over the snapshot, the open and the question"
    );
    assert_eq!(
        need_once(&sock, &once, Sel::Need(None), Since::Head),
        question,
        "and a run again pages nothing more"
    );
    assert_eq!(
        need_once(&sock, &base.join("late"), Sel::Need(None), Since::Head),
        none,
        "a notifier started now pages neither need: both stood before it"
    );
    drop(rx);
    s.quiet();
    bus.publish(NEED_FACE, s.bodies());
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the reader ends", || {
        follower.is_finished().then_some(())
    });
    follower.join().expect("the reader");
    drop(bus.conn);
    bus.server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_rise_in_a_session_older_than_the_warm_window_pages_once() {
    let dir = scratch("need-old");
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let broker = crate::test_broker(base.join("b.log")).unwrap();
    let mut bus = NeedBus::open(&base, &broker);
    let sock = bus.sock.clone();
    let mut s = Needs::open();
    let start = bus.publish(NEED_FACE, s.bodies());
    assert!(
        start.iter().any(|(_, b)| b.contains(" op=snap ")),
        "PRECONDITION: the session's start and its first snapshot"
    );
    for _ in 0..crate::notify_need::WARM_WINDOW + 10 {
        bus.row("/f/f1/pub/n-a/s-z/filler", "v=1 t=1");
    }
    let state = base.join("state");
    let run = || need_once(&sock, &state, Sel::Need(None), Since::Start);
    let none: Vec<String> = Vec::new();
    assert_eq!(run(), none, "PRECONDITION: nothing has risen");
    s.question();
    let page = bus.publish(NEED_FACE, s.bodies());
    assert!(
        page.iter().all(|(_, b)| !b.contains(" op=snap ")),
        "PRECONDITION: no snapshot seeds the session past the window"
    );
    let asked = [format!("{} question", offset_of(&page, &["k=question"]))];
    assert_eq!(
        run(),
        asked,
        "the question rises where the partial fold holds its record: it pages once"
    );
    s.quiet();
    bus.publish(NEED_FACE, s.bodies());
    assert_eq!(run(), asked, "a second run pages nothing more");
    drop(bus.conn);
    bus.server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_standing_failure_pages_once_around_a_question_that_outranks_it() {
    let dir = scratch("need-around");
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let broker = crate::test_broker(base.join("b.log")).unwrap();
    let mut bus = NeedBus::open(&base, &broker);
    let sock = bus.sock.clone();
    let failure = || Sel::Need(Some(vec!["failure"]));
    let stepwise = base.join("stepwise");
    let step = || need_once(&sock, &stepwise, failure(), Since::Start);
    let mut s = Needs::open();
    let cmd = s.cmd_open();
    s.cmd_close(cmd, aterm_slog::RunOut::Fail);
    let failed = offset_of(
        &bus.publish(NEED_FACE, s.bodies()),
        &[" op=close ", "out=fail"],
    );
    let once = [format!("{failed} failure")];
    assert_eq!(step(), once, "the failure rises");
    let q = s.question();
    let asked = offset_of(&bus.publish(NEED_FACE, s.bodies()), &["k=question"]);
    assert_eq!(step(), once, "a question outranks it, unselected");
    s.answer(q);
    let answered = bus.publish(NEED_FACE, s.bodies());
    assert!(
        answered.iter().any(|(_, b)| b.contains("out=answered")),
        "PRECONDITION: the answer reaches the bus: {answered:?}"
    );
    assert_eq!(
        step(),
        once,
        "the same failure stands again on top, and pages nothing more"
    );
    let whole = |tag: &str, on: Sel| need_once(&sock, &base.join(tag), on, Since::Start);
    assert_eq!(whole("failure", failure()), once, "read whole, once");
    let both = [format!("{failed} failure"), format!("{asked} question")];
    assert_eq!(
        whole("any", Sel::Need(None)),
        both,
        "a bare need pages each need the session raised, once"
    );
    assert_eq!(
        whole("two", Sel::Need(Some(vec!["failure", "question"]))),
        both
    );
    drop(bus.conn);
    bus.server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A NODE'S WILL KEEPS ITS SESSIONS' NEEDS (session log Phase 6 fix unit G2): a failure
/// stands, the node's will (`away` or the legacy `gone`) comes, the node and the session
/// reattach, and a handoff mark folds the same failure. Read in one run with the will, a will
/// that dropped the failure's pair would page it again; read apart, the will pages nothing,
/// and the next run's warm-up finds the failure standing.
#[cfg(unix)]
#[test]
fn a_nodes_will_keeps_its_sessions_needs_and_a_reattach_pages_nothing_again() {
    for (will, apart) in [
        ("away", false),
        ("gone", false),
        ("away", true),
        ("gone", true),
    ] {
        let dir = scratch(&format!("need-will-{will}-{apart}"));
        let base = PathBuf::from(&dir);
        std::fs::create_dir_all(&base).expect("scratch");
        let broker = crate::test_broker(base.join("b.log")).unwrap();
        let mut bus = NeedBus::open(&base, &broker);
        let sock = bus.sock.clone();
        let state = base.join("state");
        let run = || need_once(&sock, &state, Sel::Need(None), Since::Start);
        let (node, session) = (
            "/f/f1/pub/n-a/node/presence",
            "/f/f1/pub/n-a/tzzzzzzzzzx/presence",
        );
        bus.row(node, "v=1 t=1 state=live inc=1");
        bus.row(session, "v=1 t=1 inc=1 state=live");
        let mut s = Needs::open();
        let cmd = s.cmd_open();
        s.cmd_close(cmd, aterm_slog::RunOut::Fail);
        let failed = offset_of(
            &bus.publish(NEED_FACE, s.bodies()),
            &[" op=close ", "out=fail"],
        );
        let once = [format!("{failed} failure")];
        assert_eq!(run(), once, "{will}: the failure rises");
        bus.row(
            node,
            &format!("v=1 t=2 state={will} inc=1 fabric=disconnected"),
        );
        if apart {
            assert_eq!(run(), once, "{will}: the will pages nothing");
        }
        bus.row(node, "v=1 t=3 state=live inc=2");
        bus.row(session, "v=1 t=3 inc=2 state=live");
        s.quiet();
        bus.publish(NEED_FACE, s.bodies());
        assert_eq!(
            run(),
            once,
            "{will}, read {}: a reattach that restores the same failure pages it not again",
            if apart {
                "apart"
            } else {
                "in one run with the will"
            }
        );
        drop(bus.conn);
        bus.server.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn an_old_stores_word_stands_as_the_folds_pair_and_a_new_rise_still_pages() {
    let dir = scratch("need-old-store");
    let notify = Path::new(&dir).join("notify");
    std::fs::create_dir_all(&notify).expect("the store");
    std::fs::write(notify.join("standing"), format!("{NEED_FACE} failure\n"))
        .expect("an old store's one word");
    let (mut n, fired) = appending(&dir, 100, "");
    assert_eq!(
        n.standing,
        [(NEED_FACE.to_string(), "failure".to_string())],
        "PRECONDITION: the old store's line file migrated"
    );
    let sel = Sel::Need(None);
    n.cfg.on = vec![sel.clone()];
    let key = sel.key();
    let mut s = Needs::open();
    let cmd = s.cmd_open();
    s.cmd_close(cmd, aterm_slog::RunOut::Fail);
    let mut off = 0;
    for b in s.bodies() {
        off += 1;
        n.need.warm(off, NEED_FACE, b.as_bytes());
    }
    n.seed_standing(&key, off + 1);
    let mut feed = |n: &mut Notifier, bodies: Vec<String>| -> Vec<(u64, String)> {
        bodies
            .into_iter()
            .map(|b| {
                off += 1;
                n.on_record(&sel, off, NEED_FACE, b.as_bytes())
                    .expect("on_record");
                (off, b)
            })
            .collect()
    };
    s.quiet();
    feed(&mut n, s.bodies());
    assert!(fired_offsets(&fired).is_empty(), "the old word stood");
    let pairs = n.pairs_of(NEED_FACE);
    assert!(
        matches!(
            pairs.as_slice(),
            [Pair {
                word: "failure",
                raised: crate::notify_need::Raised::At { .. }
            }]
        ),
        "the fold's pair took its place: {pairs:?}"
    );
    let cmd = s.cmd_open();
    s.cmd_close(cmd, aterm_slog::RunOut::Ok);
    feed(&mut n, s.bodies());
    assert_eq!(
        standing_words(&n, NEED_FACE),
        Vec::<&str>::new(),
        "it cleared"
    );
    let cmd = s.cmd_open();
    s.cmd_close(cmd, aterm_slog::RunOut::Fail);
    let again = offset_of(&feed(&mut n, s.bodies()), &[" op=close ", "out=fail"]);
    assert_eq!(fired_offsets(&fired), [again], "a new rise pages");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Feed `bodies` to `n` as `face`'s records under `sel`, from offset `*off + 1` on: each
/// body with its offset.
fn feed_need(
    n: &mut Notifier,
    sel: &Sel,
    off: &mut u64,
    face: &str,
    bodies: Vec<String>,
) -> Vec<(u64, String)> {
    bodies
        .into_iter()
        .map(|b| {
            *off += 1;
            n.on_record(sel, *off, face, b.as_bytes())
                .expect("on_record");
            (*off, b)
        })
        .collect()
}

/// A PARTIAL FOLD'S BOUND KEEPS WHAT THE FOLD NAMES (session log Phase 6 fix unit G2,
/// review): the watch joins late (the session's first take, its start and snapshot, never
/// reaches it), then folds a failure and twenty questions asked and answered under one
/// turn, with no snapshot between. A partial fold drops no pair, so the answered questions'
/// pairs pile up past [`PAIRS_KEEP`]: the bound drops those, never the failure the fold
/// still names, which pages once.
#[test]
fn a_partial_folds_bound_keeps_the_needs_it_names_so_a_standing_failure_pages_once() {
    let dir = scratch("need-bound");
    let (mut n, fired) = appending(&dir, 100, "");
    let sel = Sel::Need(None);
    n.cfg.on = vec![sel.clone()];
    let mut off = 0;
    let mut s = Needs::open();
    let withheld = s.bodies();
    assert!(
        withheld.iter().any(|b| b.contains(" op=snap ")),
        "PRECONDITION: the withheld first take holds the start and its snapshot"
    );
    let cmd = s.cmd_open();
    s.cmd_close(cmd, aterm_slog::RunOut::Fail);
    let mut fed = feed_need(&mut n, &sel, &mut off, NEED_FACE, s.bodies());
    let mut paged = vec![offset_of(&fed, &[" op=close ", "out=fail"])];
    let turn = s.turn();
    fed.extend(feed_need(&mut n, &sel, &mut off, NEED_FACE, s.bodies()));
    for _ in 0..20 {
        let q = s.ask(turn);
        let asked = feed_need(&mut n, &sel, &mut off, NEED_FACE, s.bodies());
        paged.push(offset_of(&asked, &["k=question"]));
        s.answer(q);
        fed.extend(asked);
        fed.extend(feed_need(&mut n, &sel, &mut off, NEED_FACE, s.bodies()));
    }
    assert!(
        fed.iter().all(|(_, b)| !b.contains(" op=snap ")),
        "PRECONDITION: no snapshot seeds the fold"
    );
    assert!(
        paged.len() > PAIRS_KEEP + 1,
        "PRECONDITION: more questions page than the bound keeps"
    );
    assert_eq!(
        fired_offsets(&fired),
        paged,
        "the failure pages once, and each question once"
    );
    assert!(
        standing_words(&n, NEED_FACE).contains(&"failure"),
        "the failure still stands"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE STORE'S BOUND KEEPS A FACE THE WATCH HOLDS (G2 review, the same class): a failure
/// stands on a face the watch folds, the oldest subject written, when the store takes one
/// subject past [`STANDING_KEEP`]. The bound drops the oldest subject the watch holds no fold
/// of, so the face's next record pages nothing again.
#[test]
fn the_standing_stores_bound_drops_no_face_the_watch_holds() {
    let dir = scratch("need-keep");
    let (mut n, fired) = appending(&dir, 100, "");
    let sel = Sel::Need(None);
    n.cfg.on = vec![sel.clone()];
    let mut off = 0;
    let mut s = Needs::open();
    let cmd = s.cmd_open();
    s.cmd_close(cmd, aterm_slog::RunOut::Fail);
    let page = feed_need(&mut n, &sel, &mut off, NEED_FACE, s.bodies());
    let rose = offset_of(&page, &[" op=close ", "out=fail"]);
    assert_eq!(
        n.standing
            .iter()
            .map(|(s, _)| s.as_str())
            .collect::<Vec<_>>(),
        [NEED_FACE],
        "PRECONDITION: the face is the oldest subject written"
    );
    let other = |i: usize| {
        format!(
            "/f/f1/pub/n-b/{}/presence",
            generated_sid(u64::try_from(i).expect("bounded fixture index"))
        )
    };
    n.standing
        .extend((1..STANDING_KEEP).map(|i| (other(i), "x".to_string())));
    n.set_standing(&sel.key(), off, &other(STANDING_KEEP), Some("x"));
    s.quiet();
    feed_need(&mut n, &sel, &mut off, NEED_FACE, s.bodies());
    assert_eq!(
        fired_offsets(&fired),
        [rose],
        "the failure the face's fold still names pages not again"
    );
    assert_eq!(n.standing.len(), STANDING_KEEP);
    assert!(
        !n.standing.iter().any(|(s, _)| *s == other(1)),
        "the oldest subject the watch holds no fold of went"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// AN OWED PAGE PAGES AGAIN AFTER A RESTART (G2 review): a need whose command failed, or
/// that `--rate` dropped, is owed, so the next `--once` run's warm-up does not stand it and
/// the session's next record pages it, once, as it would in one run.
#[cfg(unix)]
#[test]
fn a_need_whose_page_failed_or_was_dropped_pages_at_its_next_record_after_a_restart() {
    let dir = scratch("need-owed");
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let broker = crate::test_broker(base.join("b.log")).unwrap();
    let mut bus = NeedBus::open(&base, &broker);
    let sock = bus.sock.clone();
    let mut x = Needs::open();
    let cmd = x.cmd_open();
    x.cmd_close(cmd, aterm_slog::RunOut::Fail);
    let failed = offset_of(
        &bus.publish(NEED_FACE, x.bodies()),
        &[" op=close ", "out=fail"],
    );

    let failing = base.join("failing");
    let (mut c, fired) = need_cfg(&sock, &failing, Sel::Need(None), Since::Start);
    c.exec = format!("{}; exit 1", c.exec);
    Notifier::new(c)
        .expect("open")
        .catch_up()
        .expect("catch up");
    let tried = fired_lines(&fired);
    assert!(
        tried.first() == Some(&format!("{failed} failure"))
            && tried.iter().all(|l| l.ends_with(" failure")),
        "PRECONDITION: the failure's command ran and failed: {tried:?}"
    );
    x.quiet();
    let quiet = bus.publish(NEED_FACE, x.bodies());
    let next = quiet.last().expect("the quiet mark").0;
    let mut once = tried.clone();
    once.push(format!("{next} failure"));
    let run = |state: &Path| need_once(&sock, state, Sel::Need(None), Since::Start);
    assert_eq!(
        run(&failing),
        once,
        "a restart pages the failed need at its session's next record"
    );
    assert_eq!(run(&failing), once, "once: it stands now");

    let y_face = "/f/f1/pub/n-a/tzzzzzzzzzy/ev";
    let mut y = Needs::open();
    let cmd = y.cmd_open();
    y.cmd_close(cmd, aterm_slog::RunOut::Fail);
    bus.publish(y_face, y.bodies());
    let dropping = base.join("dropping");
    let (mut c, fired) = need_cfg(&sock, &dropping, Sel::Need(None), Since::Start);
    c.limit = 1;
    c.window_ms = 3_600_000;
    Notifier::new(c)
        .expect("open")
        .catch_up()
        .expect("catch up");
    let budget = [format!("{failed} failure")];
    assert_eq!(
        fired_lines(&fired),
        budget,
        "PRECONDITION: tzzzzzzzzzx's failure spends the budget and --rate drops tzzzzzzzzzy's"
    );
    y.quiet();
    let quiet = bus.publish(y_face, y.bodies());
    let next = quiet.last().expect("the quiet mark").0;
    let both = [format!("{failed} failure"), format!("{next} failure")];
    assert_eq!(
        run(&dropping),
        both,
        "a restart with room pages the dropped need at its session's next record"
    );
    assert_eq!(run(&dropping), both, "once");
    drop(bus.conn);
    bus.server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every body of `bodies` but the failed run's close: the bus lost the record that raised
/// the session's `failure` (a hole), and the snapshot that ends the take is the first the
/// follower knows of it.
fn losing_the_failure(bodies: Vec<String>) -> Vec<String> {
    let kept: Vec<String> = bodies
        .iter()
        .filter(|b| !(b.contains(" op=close ") && b.contains("out=fail")))
        .cloned()
        .collect();
    assert!(
        kept.len() + 1 == bodies.len() && kept.last().is_some_and(|b| b.contains(" op=snap ")),
        "PRECONDITION: one failed close, then a snapshot naming its failure: {bodies:?}"
    );
    kept
}

/// A NEED WHOSE RAISE THE BUS LOST AND WHOSE PAGE FAILED PAGES AGAIN (fix unit J0-3's
/// lander): the failure's close is a hole, and the mail snapshot after it is where the
/// follower sees the failure rise; its command fails, so the page is owed. The session's
/// next record pages it again in the same run, as one whose raise the follower held does,
/// and so does the next record after a restart, though the fold now knows the failure only
/// By that snapshot; once a page of it ends `rc=0`, it stands.
#[cfg(unix)]
#[test]
fn a_need_whose_raise_the_bus_lost_and_whose_page_failed_pages_again_in_the_run_and_after_a_restart(
) {
    let dir = scratch("need-lost-owed");
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let broker = crate::test_broker(base.join("b.log")).unwrap();
    let mut bus = NeedBus::open(&base, &broker);
    let sock = bus.sock.clone();
    let mut s = Needs::open();
    bus.publish(NEED_FACE, s.bodies());
    s.quiet();
    bus.publish(NEED_FACE, s.bodies());
    let cmd = s.cmd_open();
    s.cmd_close(cmd, aterm_slog::RunOut::Fail);
    s.mail();
    let snap = bus
        .publish(NEED_FACE, losing_the_failure(s.bodies()))
        .last()
        .expect("the snapshot")
        .0;
    s.quiet();
    let q1 = bus.publish(NEED_FACE, s.bodies()).last().expect("quiet").0;
    let failing = base.join("failing");
    let (mut c, fired) = need_cfg(&sock, &failing, Sel::Need(None), Since::Start);
    c.exec = format!("{}; exit 1", c.exec);
    Notifier::new(c)
        .expect("open")
        .catch_up()
        .expect("catch up");
    let mut want = vec![format!("{snap} failure"), format!("{q1} failure")];
    assert_eq!(
        fired_lines(&fired),
        want,
        "the failed page runs again at the session's next record, in the same run"
    );
    s.quiet();
    let q2 = bus.publish(NEED_FACE, s.bodies()).last().expect("quiet").0;
    want.push(format!("{q2} failure"));
    let run = |state: &Path| need_once(&sock, state, Sel::Need(None), Since::Start);
    assert_eq!(
        run(&failing),
        want,
        "a restart pages it again at the session's next record"
    );
    s.quiet();
    bus.publish(NEED_FACE, s.bodies());
    assert_eq!(run(&failing), want, "once it ran rc=0, it stands");
    drop(bus.conn);
    bus.server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A SECOND FAILURE WHOSE RAISE THE BUS LOST PAGES THOUGH THE FIRST STANDS (fix unit J0-3's
/// lander): the follower paged the session's first failure at its close, and the second
/// failed run's close, which lifts the first and raises its own, is a hole; the mail
/// snapshot after it names the second. That failure was raised after a record the follower
/// read whole, so the first's pair standing on the face is another need: it pages once.
/// When that page's command fails, it is owed, and a restart pages it at the session's next
/// record, though the first failure's pair still matches the fold's.
#[cfg(unix)]
#[test]
fn a_second_failure_whose_raise_the_bus_lost_pages_though_the_first_stands() {
    for owing in [false, true] {
        let dir = scratch(if owing {
            "need-second-owed"
        } else {
            "need-second"
        });
        let base = PathBuf::from(&dir);
        std::fs::create_dir_all(&base).expect("scratch");
        let broker = crate::test_broker(base.join("b.log")).unwrap();
        let mut bus = NeedBus::open(&base, &broker);
        let sock = bus.sock.clone();
        let mut s = Needs::open();
        bus.publish(NEED_FACE, s.bodies());
        s.quiet();
        bus.publish(NEED_FACE, s.bodies());
        let cmd = s.cmd_open();
        s.cmd_close(cmd, aterm_slog::RunOut::Fail);
        let first = offset_of(
            &bus.publish(NEED_FACE, s.bodies()),
            &[" op=close ", "out=fail"],
        );
        let cmd = s.cmd_open();
        s.cmd_close(cmd, aterm_slog::RunOut::Fail);
        s.mail();
        let snap = bus
            .publish(NEED_FACE, losing_the_failure(s.bodies()))
            .last()
            .expect("the snapshot")
            .0;
        let state = base.join("st");
        let (mut c, fired) = need_cfg(&sock, &state, Sel::Need(None), Since::Start);
        if owing {
            c.exec = format!("{}; [ \"$ATERM_NOTIFY_OFFSET\" != {snap} ]", c.exec);
        }
        Notifier::new(c)
            .expect("open")
            .catch_up()
            .expect("catch up");
        let mut want = vec![format!("{first} failure"), format!("{snap} failure")];
        assert_eq!(
            fired_lines(&fired),
            want,
            "owing {owing}: each failure pages once, the second at the snapshot past its \
             lost close"
        );
        s.quiet();
        let next = bus.publish(NEED_FACE, s.bodies()).last().expect("quiet").0;
        if owing {
            want.push(format!("{next} failure"));
        }
        let run = || need_once(&sock, &state, Sel::Need(None), Since::Start);
        assert_eq!(
            run(),
            want,
            "owing {owing}: a restart pages the second failure again only when its page \
             failed"
        );
        s.quiet();
        bus.publish(NEED_FACE, s.bodies());
        assert_eq!(run(), want, "owing {owing}: then it stands");
        drop(bus.conn);
        bus.server.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// AN OWED PAGE RUNS AGAIN AT A SNAPSHOT THOUGH AN OLDER NEED OF ITS WORD STANDS (fix unit
/// J0-3's lander): the session's start never reached the bus, so the follower's fold is
/// partial and drops no entry. It holds both failed closes: the first pages, and the
/// second's command fails, so its page is owed beside the first's standing pair. After a
/// restart the session's next record is a mail snapshot, whose fold knows the failure only
/// By that snapshot, which the first failure's pair matches as well as the owed one: the
/// owed page runs again there, as the session's next record of any other kind would run it.
#[cfg(unix)]
#[test]
fn an_owed_page_runs_again_at_a_snapshot_though_an_older_need_of_its_word_stands() {
    let dir = scratch("need-owed-snap");
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let broker = crate::test_broker(base.join("b.log")).unwrap();
    let mut bus = NeedBus::open(&base, &broker);
    let sock = bus.sock.clone();
    let mut s = Needs::open();
    let withheld = s.bodies();
    assert!(
        withheld.iter().any(|b| b.contains(" op=snap ")),
        "PRECONDITION: the session's start and first snapshot never reach the bus"
    );
    let close = [" op=close ", "out=fail"];
    let cmd = s.cmd_open();
    s.cmd_close(cmd, aterm_slog::RunOut::Fail);
    let first = offset_of(&bus.publish(NEED_FACE, s.bodies()), &close);
    let cmd = s.cmd_open();
    s.cmd_close(cmd, aterm_slog::RunOut::Fail);
    let second = offset_of(&bus.publish(NEED_FACE, s.bodies()), &close);
    let state = base.join("st");
    let (mut c, fired) = need_cfg(&sock, &state, Sel::Need(None), Since::Start);
    c.exec = format!("{}; [ \"$ATERM_NOTIFY_OFFSET\" != {second} ]", c.exec);
    Notifier::new(c)
        .expect("open")
        .catch_up()
        .expect("catch up");
    let mut want = vec![format!("{first} failure"), format!("{second} failure")];
    assert_eq!(
        fired_lines(&fired),
        want,
        "PRECONDITION: each failure's page ran, the second's failed"
    );
    s.mail();
    let snapped = bus.publish(NEED_FACE, s.bodies());
    assert!(
        snapped.len() == 1 && snapped[0].1.contains(" op=snap "),
        "PRECONDITION: the mail's snapshot alone: {snapped:?}"
    );
    want.push(format!("{} failure", snapped[0].0));
    let run = || need_once(&sock, &state, Sel::Need(None), Since::Start);
    assert_eq!(
        run(),
        want,
        "a restart runs the owed page at the snapshot, though the first failure stands"
    );
    s.quiet();
    bus.publish(NEED_FACE, s.bodies());
    assert_eq!(run(), want, "then it stands");
    drop(bus.conn);
    bus.server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// AN OWED PAGE WAITS FOR A FOLD THAT STILL SEES ITS NEED (fix unit J0-3's lander): the
/// session's start never reached the bus, and the follower holds the failed close, whose
/// page fails. The bus then loses a record after it (a hole), which may have lifted it: the
/// partial fold of the next record no longer sees the failure, and a restart runs nothing.
/// A mail snapshot that names the failure, with nothing lost past it, sees it again, and the
/// owed page runs there.
#[cfg(unix)]
#[test]
fn an_owed_page_waits_for_a_fold_that_still_sees_its_need() {
    let dir = scratch("need-owed-seen");
    let base = PathBuf::from(&dir);
    std::fs::create_dir_all(&base).expect("scratch");
    let broker = crate::test_broker(base.join("b.log")).unwrap();
    let mut bus = NeedBus::open(&base, &broker);
    let sock = bus.sock.clone();
    let mut s = Needs::open();
    let withheld = s.bodies();
    assert!(
        withheld.iter().any(|b| b.contains(" op=snap ")),
        "PRECONDITION: the session's start and first snapshot never reach the bus"
    );
    let cmd = s.cmd_open();
    s.cmd_close(cmd, aterm_slog::RunOut::Fail);
    let first = offset_of(
        &bus.publish(NEED_FACE, s.bodies()),
        &[" op=close ", "out=fail"],
    );
    let state = base.join("st");
    let (mut c, fired) = need_cfg(&sock, &state, Sel::Need(None), Since::Start);
    c.exec = format!("{}; [ \"$ATERM_NOTIFY_OFFSET\" != {first} ]", c.exec);
    let failing = c.exec.clone();
    Notifier::new(c)
        .expect("open")
        .catch_up()
        .expect("catch up");
    let mut want = vec![format!("{first} failure")];
    assert_eq!(
        fired_lines(&fired),
        want,
        "PRECONDITION: the failure's page ran and failed"
    );
    let run = || {
        let (mut c, fired) = need_cfg(&sock, &state, Sel::Need(None), Since::Start);
        c.exec.clone_from(&failing);
        Notifier::new(c)
            .expect("open")
            .catch_up()
            .expect("catch up");
        fired_lines(&fired)
    };
    s.quiet();
    s.quiet();
    let quiet = s.bodies();
    assert_eq!(quiet.len(), 2, "PRECONDITION: two records");
    bus.publish(NEED_FACE, quiet[1..].to_vec());
    assert_eq!(
        run(),
        want,
        "a fold that lost a record after the failure does not see it, and runs nothing"
    );
    s.mail();
    let snapped = bus.publish(NEED_FACE, s.bodies());
    assert!(
        snapped.len() == 1 && snapped[0].1.contains(" op=snap "),
        "PRECONDITION: the mail's snapshot alone: {snapped:?}"
    );
    want.push(format!("{} failure", snapped[0].0));
    assert_eq!(
        run(),
        want,
        "the snapshot sees it again, and the owed page runs"
    );
    s.quiet();
    bus.publish(NEED_FACE, s.bodies());
    assert_eq!(run(), want, "then it stands");
    drop(bus.conn);
    bus.server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn need_selectors_take_the_needs_a_fold_raises_and_refuse_any_other() {
    assert_eq!(Sel::parse("need"), Ok(Sel::Need(None)));
    let words: Vec<&str> = aterm_slog::observed_need_words().collect();
    let families: Vec<&str> = need_families().collect();
    for w in &words {
        assert_eq!(
            Sel::parse(&format!("need:{w}")),
            Ok(Sel::Need(Some(vec![*w]))),
            "{w}"
        );
    }
    for f in &families {
        let sel = Sel::parse(&format!("need:{f}")).expect("a family");
        assert!(
            words.iter().any(|w| sel.selects(w)),
            "{f}: a family names some word"
        );
        for w in &words {
            // `wall` is a word too: a wall whose kind nothing names. `need:wall` takes it
            // with every `wall:<kind>`.
            assert_eq!(
                sel.selects(w),
                w == f || w.split_once(':').is_some_and(|(family, _)| family == *f),
                "need:{f} selects {w} exactly when it is of the family, or is its bare word"
            );
        }
    }
    assert!(
        Sel::parse("need:approval")
            .expect("approval is a family")
            .selects("approval:bash"),
        "need:approval takes an approval:bash rise"
    );
    let never: Vec<&str> = aterm_slog::vocab::NEED_WORDS
        .iter()
        .copied()
        .filter(|w| !words.contains(w) && !families.contains(w))
        .collect();
    for named in ["ask", "review", "decision", "help", "memory", "stalled"] {
        assert!(
            never.contains(&named),
            "PRECONDITION: the log spells {named}, which no fold raises as observed"
        );
    }
    for bad in never.iter().map(|w| format!("need:{w}")).chain(
        [
            "need:nope",
            "need:",
            "need:FAILURE",
            "need: failure",
            "need:approval:",
            "needs",
        ]
        .map(str::to_string),
    ) {
        let e = Sel::parse(&bad).expect_err(&bad);
        if let Some(word) = bad.strip_prefix("need:") {
            assert!(e.starts_with(&format!("--on need:{word}: ")), "{e}");
            for w in words.iter().chain(&families) {
                assert!(
                    e.split(['(', ' ', ')', ';']).any(|t| t == *w),
                    "{bad}: the error names {w}: {e}"
                );
            }
        }
    }
    let args = |s: &str| -> Vec<String> { s.split(' ').map(str::to_string).collect() };
    let base = "--fleet f1 --broker /tmp/b.sock --state /tmp/x --exec true";
    let on = |rest: &str| parse(&args(&format!("{base} {rest}"))).expect("parse").on;
    assert_eq!(
        on("--on need:failure,attention,need:question,need:failure"),
        [Sel::Need(Some(vec!["failure", "question"])), Sel::Attention],
        "one need selector, its words in first-seen order"
    );
    assert_eq!(
        on("--on need:failure --on need"),
        [Sel::Need(None)],
        "a bare need takes every word"
    );
    assert_eq!(on("--on need,need:failure"), [Sel::Need(None)]);
    assert_eq!(Sel::Need(None).key(), "need");
    assert_eq!(Sel::Need(None).filter("f1"), "/f/f1/pub/*/*/*");
}

#[test]
fn the_help_names_exactly_the_needs_a_fold_raises_and_their_families() {
    let help = usage();
    let (words, families) = (fill(need_words()), fill(need_families()));
    assert_eq!(
        words.split_whitespace().collect::<Vec<_>>(),
        aterm_slog::observed_need_words().collect::<Vec<_>>(),
        "the list is the needs a fold raises, in the log's order"
    );
    let spelled: Vec<&str> = aterm_slog::observed_need_words()
        .filter_map(|w| w.split_once(':').map(|(family, _)| family))
        .collect();
    let listed: Vec<&str> = families.split_whitespace().collect();
    assert!(
        !listed.is_empty() && listed.iter().all(|f| spelled.contains(f)),
        "each family is one the words spell: {listed:?}"
    );
    assert!(
        spelled.iter().all(|f| listed.contains(f)),
        "and every family they spell is listed: {listed:?}"
    );
    for list in [&words, &families] {
        assert!(
            help.contains(&format!("\n{list}\n")),
            "the help prints that list whole: {help}"
        );
    }
    assert!(help.contains("need[:<word>]"), "{help}");
}

#[test]
fn the_rate_flag_refuses_a_budget_that_would_drop_everything() {
    assert_eq!(parse_rate("10/1m"), Ok((10, 60_000)));
    assert_eq!(parse_rate("1/30s"), Ok((1, 30_000)));
    assert_eq!(parse_rate("40/2h"), Ok((40, 7_200_000)));
    assert!(
        parse_rate("0/1m").is_err(),
        "a zero limit is refused by name"
    );
    assert!(parse_rate("10/0m").is_err(), "a zero window is refused");
    for bad in ["10", "10/1d", "x/1m", "10/m", ""] {
        assert!(parse_rate(bad).is_err(), "{bad}");
    }
}

#[test]
fn rate_windows_refuse_overflow_and_non_ascii_units() {
    for (unit, scale) in [('s', 1_000), ('m', 60_000), ('h', 3_600_000)] {
        let largest = u64::MAX / scale;
        assert_eq!(
            parse_rate(&format!("1/{largest}{unit}")),
            Ok((1, largest * scale))
        );
        assert!(parse_rate(&format!("1/{}{unit}", largest + 1)).is_err());
    }
    for bad in ["1/1é", "1/日", "1/1\u{1f600}", "1/"] {
        assert!(parse_rate(bad).is_err(), "{bad}");
    }
}

#[test]
fn argv_requires_a_watch_and_a_command() {
    let args = |s: &str| -> Vec<String> { s.split(' ').map(str::to_string).collect() };
    let base = "--fleet f1 --broker /tmp/b.sock --state /tmp/x --exec true";
    let cfg = parse(&args(&format!("{base} --on attention,halt,attention"))).expect("parse");
    assert_eq!(
        cfg.on,
        vec![Sel::Attention, Sel::Halt],
        "a repeat is folded"
    );
    assert_eq!(
        (cfg.limit, cfg.window_ms),
        (DEFAULT_LIMIT, DEFAULT_WINDOW_MS)
    );
    assert_eq!(
        cfg.since,
        Since::Head,
        "a fresh install does not page history"
    );
    assert!(!cfg.once);
    assert!(
        parse(&args("--fleet f1 --broker b --exec true")).is_err(),
        "no --on"
    );
    assert!(
        parse(&args("--fleet f1 --broker b --on halt")).is_err(),
        "no --exec"
    );
    assert!(
        parse(&args(&format!("{base} --on halt --rate"))).is_err(),
        "a flag needs a value"
    );
}

#[test]
fn a_carry_refuses_incomplete_cursor_and_verdict_reads() {
    let dir = crate::test_scratch_dir("carry-ref");
    let from = Store::at(dir.path().join("from")).unwrap();
    std::fs::create_dir_all(from.root.join("cur")).unwrap();
    from.set_cursor("attention", 2).unwrap();
    from.set_cursor("ask-to.h-alice", 9).unwrap();
    from.set_cursor("ask-to.h-bob", 4).unwrap();
    from.set_journal(
        &["1 fired rc=0".into(), "7 fired rc=0".into()],
        Flush::Ordered,
    )
    .unwrap();
    from.set_standing(&[("/f/f/pub/n-a/s-a/presence".into(), "help".into())])
        .unwrap();
    let logs_store = Store::at(dir.path().join("target"))
        .unwrap()
        .child("logs", true)
        .unwrap();
    let logs = logs_store.root.clone();
    let ordinary = logs.join("ordinary");
    logs_store.carry(&from.read_only(), "ordinary", 6).unwrap();
    let carried = Store::<ReadOnly>::existing(ordinary).unwrap();
    assert_eq!(carried.read_cursor("attention"), Some(2));
    assert_eq!(carried.read_cursor("ask-to.h-alice"), Some(6));
    assert_eq!(carried.read_cursor("ask-to.h-bob"), Some(4));
    std::fs::create_dir_all(carried.root.join("cur")).unwrap();
    std::fs::write(carried.root.join("cur/attention.migrated"), "3\n").unwrap();
    std::os::unix::fs::symlink(
        carried.root.join("cur/attention.migrated"),
        carried.root.join("cur/halt"),
    )
    .unwrap();
    assert_eq!(carried.read_cursor("halt"), None, "a link is no cursor");
    std::fs::remove_dir_all(carried.root.join("cur")).unwrap();
    assert_eq!(
        carried
            .read_verdicts()
            .unwrap()
            .lines
            .into_iter()
            .collect::<Vec<_>>(),
        ["1 fired rc=0"]
    );
    assert_eq!(carried.read_verdicts().unwrap().standing, from.standing());
    let empty = Store::at(dir.path().join("empty")).unwrap();
    std::fs::create_dir_all(&empty.root).unwrap();
    logs_store.carry(&empty.read_only(), "empty", 6).unwrap();

    let mut refused = Vec::new();
    for (n, bytes) in [b"not an offset".as_slice(), b"", b"\xff"]
        .into_iter()
        .enumerate()
    {
        std::fs::write(from.root.join("cur/attention"), bytes).unwrap();
        let to = logs.join(format!("bad-cursor-{n}"));
        let result = logs_store.carry(
            &from.read_only(),
            to.file_name().unwrap().to_str().unwrap(),
            6,
        );
        refused.push(
            result.is_err()
                && !to.exists()
                && std::fs::read(from.root.join("cur/attention")).is_ok_and(|got| got == bytes),
        );
    }
    from.set_cursor("attention", 2).unwrap();
    let path = from.root.join(JOURNAL_FILE);
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let to = logs.join("unreadable-journal");
    let result = logs_store.carry(
        &from.read_only(),
        to.file_name().unwrap().to_str().unwrap(),
        6,
    );
    refused.push(result.is_err() && !to.exists() && path.is_dir());
    std::fs::remove_dir(&path).unwrap();
    from.set_journal(&[], Flush::Ordered).unwrap();
    let steps: Vec<journal::Step> = (1..=3)
        .map(|off| journal::Step {
            verdict: Some(format!("{off} fired rc=0")),
            ..journal::Step::default()
        })
        .collect();
    let (mut damaged, _) = journal::encode_from(0, &steps).unwrap();
    let (first, _) = journal::encode_from(0, &steps[..1]).unwrap();
    damaged[first.len() + 20] ^= 0x55;
    let path = from.root.join(JOURNAL_FILE);
    std::fs::write(&path, &damaged).unwrap();
    let to = logs.join("faulted-journal");
    let result = logs_store.carry(
        &from.read_only(),
        to.file_name().unwrap().to_str().unwrap(),
        6,
    );
    refused.push(
        result.is_err()
            && !to.exists()
            && std::fs::read(&path).is_ok_and(|got| got == damaged)
            && !from.root.join(journal::FAULT).exists(),
    );
    assert!(
        refused.into_iter().all(|held| held),
        "CARRY_PUBLISHED_INCOMPLETE_EVIDENCE"
    );
}

/// AN OLD STORE'S UNREADABLE LINE FILE FAILS NO CARRY: a predecessor `logs/<old>/` store of a
/// build before the journal, whose `rate` is not text, whose `suppressed` is not a regular file
/// and whose `standing` is not text, is carried as a load would read it, each of those as empty
/// and its readable `journal` below the fork, and the source is left as it was: nothing set
/// aside, no journal written. A strict read would refuse the carry, and the switch to the new
/// log, at every retry.
#[test]
fn a_carry_reads_an_old_stores_unreadable_line_files_as_empty_and_leaves_them() {
    let dir = crate::test_scratch_dir("carry-lenient");
    let from = Store::at(dir.path().join("from")).unwrap();
    std::fs::create_dir_all(from.root.join("cur")).unwrap();
    std::fs::write(from.root.join("cur/attention"), "9\n").unwrap();
    std::fs::write(
        from.root.join("journal"),
        "1 fired on=attention rc=0 dup=0\n7 fired on=attention rc=0 dup=0\n",
    )
    .unwrap();
    std::fs::write(from.root.join("rate"), b"\xff\xfe not text").unwrap();
    std::fs::create_dir(from.root.join("suppressed")).unwrap();
    std::fs::write(from.root.join("standing"), b"\xff not text").unwrap();
    let names = |root: &Path| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let before = names(&from.root);
    let logs_store = Store::at(dir.path().join("target"))
        .unwrap()
        .child("logs", true)
        .unwrap();
    let to = logs_store.root.join("next");
    logs_store
        .carry(
            &from.read_only(),
            to.file_name().unwrap().to_str().unwrap(),
            6,
        )
        .expect("an unreadable line file is read as empty");
    let carried = Store::<ReadOnly>::existing(to).unwrap();
    assert_eq!(carried.read_cursor("attention"), Some(6));
    assert_eq!(
        carried
            .read_verdicts()
            .unwrap()
            .lines
            .into_iter()
            .collect::<Vec<_>>(),
        ["1 fired on=attention rc=0 dup=0"]
    );
    assert_eq!(
        carried.read_verdicts().unwrap().standing,
        Vec::<(String, String)>::new()
    );
    assert_eq!(names(&from.root), before, "the source is left as it was");
    assert_eq!(
        std::fs::read(from.root.join("rate")).unwrap(),
        b"\xff\xfe not text"
    );
    assert!(from.root.join("suppressed").is_dir());
}

/// A line-format `standing` file that does not read says no standing values: the journal's
/// stand, so a carry from that store, and a load of it, keep what the journal holds (a load
/// sets the file aside). NEGATIVE CONTROL: a `standing` file that reads replaces them.
#[test]
fn an_unreadable_standing_file_keeps_the_journals_standing_values() {
    let dir = crate::test_scratch_dir("carry-standing");
    let from = Store::at(dir.path().join("from")).unwrap();
    std::fs::create_dir_all(from.root.join("cur")).unwrap();
    from.set_cursor("attention", 9).unwrap();
    let rows = vec![("/f/f1/pub/n-a/s-one/presence".to_owned(), "key".to_owned())];
    from.set_standing(&rows).unwrap();
    std::fs::create_dir(from.root.join(journal::STANDING)).unwrap();
    assert_eq!(from.standing(), rows);
    let logs_store = Store::at(dir.path().join("target"))
        .unwrap()
        .child("logs", true)
        .unwrap();
    let to = logs_store.root.join("next");
    logs_store
        .carry(
            &from.read_only(),
            to.file_name().unwrap().to_str().unwrap(),
            6,
        )
        .expect("carry");
    assert_eq!(
        Store::<ReadOnly>::existing(to)
            .unwrap()
            .read_verdicts()
            .unwrap()
            .standing,
        rows,
        "the carry keeps them"
    );
    assert!(from.root.join(journal::STANDING).is_dir(), "left as it was");
    assert_eq!(
        from.verdicts().unwrap().standing,
        rows,
        "the load keeps them"
    );
    assert!(
        !from.root.join(journal::STANDING).exists()
            && from
                .root
                .join(format!("{}{}", journal::STANDING, journal::SET_ASIDE))
                .is_dir(),
        "the load sets it aside"
    );
    std::fs::write(
        from.root.join(journal::STANDING),
        "/f/f1/pub/n-a/s-two/presence other\n",
    )
    .unwrap();
    assert_ne!(
        from.standing(),
        rows,
        "CONTROL: one that reads replaces them"
    );
}

#[test]
fn a_carry_retires_only_its_exclusively_owned_stage() {
    let dir = crate::test_scratch_dir("carry-own");
    let from = Store::at(dir.path().join("from")).unwrap();
    std::fs::create_dir_all(from.root.join("cur")).unwrap();
    from.set_cursor("attention", 2).unwrap();
    let logs_store = Store::at(dir.path().join("target"))
        .unwrap()
        .child("logs", true)
        .unwrap();
    let logs = logs_store.root.clone();

    let foreign = logs.join(format!(".carry-{}-0", std::process::id()));
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(foreign.join("owner"), b"standing owner").unwrap();
    let target = logs.join("landed");
    logs_store.carry(&from.read_only(), "landed", 6).unwrap();
    assert_eq!(
        Store::<ReadOnly>::existing(target)
            .unwrap()
            .read_cursor("attention"),
        Some(2)
    );
    let winner = logs.join("winner");
    std::fs::create_dir_all(winner.join("cur")).unwrap();
    std::fs::write(winner.join("cur/attention"), b"4").unwrap();
    logs_store.carry(&from.read_only(), "winner", 6).unwrap();
    assert_eq!(std::fs::read(winner.join("cur/attention")).unwrap(), b"4");
    let blocked = logs.join("blocked");
    std::fs::write(&blocked, b"do not replace").unwrap();
    assert!(logs_store.carry(&from.read_only(), "blocked", 6).is_err());
    assert_eq!(std::fs::read(&blocked).unwrap(), b"do not replace");
    assert_eq!(
        std::fs::read(foreign.join("owner")).unwrap(),
        b"standing owner"
    );
    let stages: Vec<_> = std::fs::read_dir(&logs)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".carry-")
        })
        .collect();
    assert_eq!(stages, [foreign], "only the pre-existing owner remains");
}

/// One `halt` page's flushes, as `aterm_durable` was asked for them: before its command (its
/// `firing` step), then after it (its `fired` step). A `halt` page leaves nothing standing and is
/// judged here with no cursor written, so these are the journal's and the stamps' alone.
fn halt_page_flushes(n: &mut Notifier, off: u64) -> [Vec<Flush>; 2] {
    let (before, recording) = flushes_by_command();
    n.on_record(
        &Sel::Halt,
        off,
        "/f/f1/fleet/h-andrew/halt",
        b"v=1 t=1 state=on reason=stop",
    )
    .expect("a page");
    AT_EXEC.with(|seam| seam.borrow_mut().take());
    let mut before = before.take();
    assert_eq!(before.len(), 1, "PRECONDITION: its command ran once");
    [before.pop().unwrap_or_default(), recording.take()]
}

/// A PAGE'S JOURNAL AND STAMP WRITES COST ITS ONE FULL FLUSH AND AT MOST TWO BARRIERS: in the
/// root's store its `firing` step (the line and the stamp it spends) appended and flushed in
/// full, its `fired` step appended and ordered; in a log's own store the line's step ordered
/// ahead of the stamp's, whose flush is the full one; and where a step falls past the journal's
/// bound, that step a rewrite (the replace ordered, its directory flushed as the step would
/// have been). The NEGATIVE CONTROL is a journal rewritten whole at every write, as the line
/// files were: past the two barriers.
#[test]
fn a_pages_journal_and_stamp_writes_cost_one_full_flush_and_at_most_two_barriers() {
    use Flush::{Durable as D, Ordered as O};
    /// A case: its name, whether it reads a log's own store, how far past its steps the
    /// journal's bound is set before the page (none, or 0 for its `firing` step and 1 for its
    /// `fired` step to fall past it), and what the page asks for before and after its command.
    type Case = (&'static str, bool, Option<u64>, [&'static [Flush]; 2]);
    let cases: [Case; 4] = [
        ("the root's store", false, None, [&[D], &[O]]),
        ("a log's own store", true, None, [&[O, D], &[O]]),
        (
            "a `firing` step past the bound",
            false,
            Some(0),
            [&[O, D], &[O]],
        ),
        (
            "a `fired` step past the bound",
            false,
            Some(1),
            [&[D], &[O, O]],
        ),
    ];
    for mutant in [false, true] {
        for (at, (case, own_store, bound, asks)) in cases.iter().enumerate() {
            let dir = scratch(&format!("page-cost-{mutant}-{at}"));
            if *own_store {
                let _ = Store::open(&dir)
                    .expect("open")
                    .for_log(
                        Some(astream_broker::LogIdentity {
                            id: [8; 16],
                            generation: 0,
                        }),
                        None,
                        &Steps::default(),
                    )
                    .expect("the root's store claimed by another log");
            }
            let (mut n, _) = appending(&dir, 100, "");
            n.cfg.on = vec![Sel::Halt];
            n.scope_to(&strict_log()).expect("the log's store");
            halt_page_flushes(&mut n, 1);
            let file = n.log_file.as_mut().unwrap_or(&mut n.root_file);
            if let Some(past) = bound {
                file.bound.steps = file.steps + past;
                file.bound.bytes = 0;
            }
            file.rewrite_always = mutant;
            n.root_file.rewrite_always = mutant;
            let got = halt_page_flushes(&mut n, 2);
            let all: Vec<Flush> = got.iter().flatten().copied().collect();
            let barriers = all.iter().filter(|f| f.is_ordered()).count();
            if mutant {
                assert!(
                    barriers > 2,
                    "NEGATIVE CONTROL, {case}: a journal rewritten whole at every write costs \
                     past two barriers: {got:?}"
                );
            } else {
                assert_eq!(
                    [got[0].as_slice(), got[1].as_slice()],
                    *asks,
                    "{case}: before its command, then after it"
                );
                assert_eq!(
                    (
                        all.iter().filter(|f| f.is_durable()).count(),
                        got[0].last(),
                        barriers <= 2
                    ),
                    (1, Some(&D), true),
                    "{case}: one full flush, the last before its command, and at most two \
                     barriers: {got:?}"
                );
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// The bytes this thread's notify stores wrote while `run` ran ([`BYTES_NOTED`]).
fn bytes_of(run: impl FnOnce()) -> u64 {
    let from = BYTES_NOTED.with(std::cell::Cell::get);
    run();
    BYTES_NOTED.with(std::cell::Cell::get) - from
}

/// The bytes each record a drop flood drops may cost, amortized: its own step (about 160 bytes
/// with its frame and its count) and its share of the rewrites the journal's bound brings.
const DROP_BYTES_MAX: u64 = 512;

/// A DROP FLOOD WRITES BYTES IN PROPORTION TO ITS DROPS: 1,000 records past `--rate`, into an
/// empty journal and into a full one ([`JOURNAL_KEEP`] verdicts), each write at most
/// [`DROP_BYTES_MAX`] a drop, where the line files rewrote the whole journal and the count at
/// every drop (the NEGATIVE CONTROL, a journal rewritten whole at every write: past the bound
/// by far). The tally a fabric bench reads says the bytes too ([`FLUSH_TALLY`]'s `bytes=`).
#[test]
fn a_drop_flood_writes_bytes_in_proportion_to_its_drops() {
    const DROPS: u64 = 1_000;
    let bound = DROPS * DROP_BYTES_MAX;
    for mutant in [false, true] {
        for full in [false, true] {
            if mutant && full {
                continue;
            }
            let dir = scratch(&format!("drop-flood-{mutant}-{full}"));
            let (mut n, fired) = appending(&dir, 1, "");
            n.wall_clock = Some(1_000);
            feed(&mut n, 1, "t0000000000", "v=1 t=1 attention=paged");
            if full {
                let mut off = 2;
                let fill_until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
                while n.root_file.steps + 100 < n.root_file.bound.steps {
                    assert!(
                        Instant::now() < fill_until,
                        "the journal did not fill within the one-minute hang detector: {} steps of {}",
                        n.root_file.steps,
                        n.root_file.bound.steps
                    );
                    feed(
                        &mut n,
                        off,
                        &generated_sid(off),
                        "v=1 t=1 attention=filling",
                    );
                    off += 1;
                }
                assert_eq!(
                    n.journal.len(),
                    JOURNAL_KEEP,
                    "PRECONDITION: a full journal, 100 steps short of its bound"
                );
            }
            n.root_file.rewrite_always = mutant;
            let (dropped, steps) = (n.counts.dropped, n.root_file.steps);
            let wrote = bytes_of(|| {
                for off in 100_000..100_000 + DROPS {
                    feed(&mut n, off, &generated_sid(off), "v=1 t=1 attention=flood");
                }
            });
            assert_eq!(
                (fired_offsets(&fired), n.counts.dropped - dropped),
                (vec![1], DROPS),
                "PRECONDITION: a budget of 1 pages the first record and drops the flood"
            );
            eprintln!(
                "drop flood (full={full}, whole rewrite at every write={mutant}): {DROPS} drops \
                 wrote {wrote} bytes, {} a drop",
                wrote / DROPS
            );
            assert!(
                !full || n.root_file.steps < steps + DROPS,
                "PRECONDITION: the full journal's flood passed its bound and was rewritten"
            );
            if mutant {
                assert!(
                    wrote > 10 * bound,
                    "NEGATIVE CONTROL: rewritten whole at every drop, {DROPS} drops write \
                     {wrote} bytes, within ten times the bound {bound}"
                );
            } else {
                assert!(
                    wrote <= bound,
                    "{DROPS} drops (full={full}) wrote {wrote} bytes, past {bound}"
                );
                let tally = n.store.root.join(FLUSH_TALLY);
                n.flush_tally = Some((u64::MAX, u64::MAX, u64::MAX));
                n.tell();
                let text = std::fs::read_to_string(&tally).expect("the tally");
                assert!(
                    text.split_whitespace()
                        .filter_map(|w| w.strip_prefix("bytes="))
                        .any(|b| b.parse::<u64>().is_ok_and(|b| b >= wrote)),
                    "the tally tells the bytes: {text}"
                );
                let (held, cfg) = ((n.journal.clone(), n.suppressed), n.cfg.clone());
                drop(n);
                let back = Notifier::new(cfg).expect("reopen");
                assert_eq!(
                    (back.journal.clone(), back.suppressed),
                    held,
                    "the journal reads back as the notifier holds it (full={full})"
                );
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// The bytes each session whose needs a flood drops may cost, amortized (about 370 measured):
/// its `dropped` steps (about 170 bytes each with its frame and its count), its standing
/// change's step with its selector's cursor in it, and its share of the rewrites
/// the journal's bound brings.
const NEED_DROP_BYTES_MAX: u64 = 512;

/// A FLOOD OF DISTINCT NEED DROPS WRITES BYTES IN PROPORTION TO ITS DROPS (ATERM-KI-1111-11):
/// 500 sessions each raise a failure past `--rate`, so each drop owes its pair, a change of the
/// standing values, and the flood writes at most [`NEED_DROP_BYTES_MAX`] a session, where the line
/// file replaced every value standing at every change (the NEGATIVE CONTROL, each change
/// replacing the line-format `standing` file whole, the write a build before the steps made:
/// past the bound by far). Reopened, the store holds the values as the notifier does, each drop
/// owed (the mutant's line file migrated at the open).
#[test]
fn a_flood_of_distinct_need_drops_writes_bytes_in_proportion_to_its_drops() {
    const SESSIONS: u64 = 500;
    let bound = SESSIONS * NEED_DROP_BYTES_MAX;
    for mutant in [false, true] {
        let dir = scratch(&format!("need-drop-flood-{mutant}"));
        let (mut n, fired) = appending(&dir, 1, "");
        n.wall_clock = Some(1_000);
        let sel = Sel::Need(None);
        n.cfg.on = vec![sel.clone()];
        n.standing_whole = mutant;
        let face = |i: u64| format!("/f/f1/pub/n-a/{}/ev", generated_sid(i));
        let mut off = 0;
        let mut first = Needs::open();
        let cmd = first.cmd_open();
        first.cmd_close(cmd, aterm_slog::RunOut::Fail);
        feed_need(&mut n, &sel, &mut off, &face(0), first.bodies());
        let (dropped, changes) = (n.counts.dropped, n.standing.len());
        let wrote = bytes_of(|| {
            for i in 1..=SESSIONS {
                let mut s = Needs::open();
                let cmd = s.cmd_open();
                s.cmd_close(cmd, aterm_slog::RunOut::Fail);
                feed_need(&mut n, &sel, &mut off, &face(i), s.bodies());
            }
        });
        let drops = n.counts.dropped - dropped;
        assert_eq!(
            (fired_offsets(&fired).len(), n.standing.len() - changes),
            (1, usize::try_from(SESSIONS).expect("small")),
            "PRECONDITION: a budget of 1 pages the first failure, and each other session's \
             needs are dropped and owed on its own face"
        );
        assert!(drops >= SESSIONS, "PRECONDITION: {drops} drops");
        eprintln!(
            "need drop flood (the line file replaced at every change={mutant}): {SESSIONS} sessions, \
             {drops} drops, wrote {wrote} bytes, {} a session",
            wrote / SESSIONS
        );
        if mutant {
            assert!(
                wrote > 10 * bound,
                "NEGATIVE CONTROL: the line file replaced at every change, {SESSIONS} drops \
                 write {wrote} bytes, within ten times the bound {bound}"
            );
        } else {
            assert!(
                wrote <= bound,
                "{SESSIONS} distinct need drops wrote {wrote} bytes, past {bound}"
            );
        }
        let (standing, cfg) = (n.standing.clone(), n.cfg.clone());
        drop(n);
        let back = Notifier::new(cfg).expect("reopen");
        assert_eq!(
            back.standing, standing,
            "the standing values read back as the notifier holds them (mutant={mutant})"
        );
        assert!(
            (1..=SESSIONS).all(|i| back.entries_of(&face(i)).iter().all(|e| e.owed)),
            "each dropped need is owed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A frozen line-format store (`journal`, `rate`, `suppressed`, `standing`, as a build before the
/// journal wrote them, `tests/fixtures/notify-line-format/`) migrates at the notifier's open:
/// its verdicts, stamps and count read as they were, and its pending page (`5 firing`, whose
/// command a kill cut) runs again with `ATERM_NOTIFY_DUP=1`, while a page it holds `fired` is
/// skipped.
#[test]
fn a_frozen_line_format_journal_migrates_and_its_pending_page_runs_again() {
    let dir = scratch("frozen");
    journal::tests::lay_frozen_store(&Path::new(&dir).join("notify"));
    let (mut n, fired) = appending(&dir, 2, "");
    assert_eq!(
        Vec::from(n.journal.clone()),
        journal::tests::frozen_lines(),
        "the verdicts migrated"
    );
    assert_eq!(
        (n.stamps.clone(), n.suppressed, n.suppressed_mark),
        (
            journal::tests::FROZEN_STAMPS.to_vec(),
            journal::tests::FROZEN_SUPPRESSED.0,
            journal::tests::FROZEN_SUPPRESSED.1
        ),
        "the stamps and the count migrated"
    );
    feed(&mut n, 1, "tzzzzzzzzza", "v=1 t=9 inc=2 attention=again");
    feed(&mut n, 5, "tzzzzzzzzze", "v=1 t=9 attention=pending");
    assert_eq!(
        fired_offsets(&fired),
        [5],
        "the pending page ran again, the fired one not"
    );
    assert_eq!(
        (n.counts.dup, n.counts.skipped, n.verdict(5)),
        (1, 1, Some("fired")),
        "as a duplicate"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The ordered flushes each session whose needs a flood drops may ask for (measured: 5, its
/// two `dropped` steps, the standing change's step that carries its selector's cursor at the
/// record, and the cursor's checkpoint past each record that wrote a verdict, a step each),
/// where the cursor written as a file of its own, `cur/<key>` replaced and its directory
/// ordered (two flushes) ahead of the change and at each checkpoint, asked for 9.
const NEED_DROP_FLUSHES_MAX: usize = 5;

/// A DISTINCT NEED DROP'S CURSOR IS A STEP OF THE CHANGE IT ORDERS (ATERM-KI-1111-11): 100
/// sessions each raise a failure past `--rate`, each read through the follower's path
/// ([`Notifier::take`], so each record's verdict moves its selector's cursor), and each
/// session asks for at most [`NEED_DROP_FLUSHES_MAX`] ordered flushes and no full one. The
/// NEGATIVE CONTROL writes the cursor as a file of its own ahead of the standing change and at
/// each checkpoint (`Notifier::cursor_file`, the write a build before the cursor steps made),
/// and asks for more. Reopened, the store holds the cursor the notifier wrote.
#[test]
fn a_distinct_need_drops_cursor_is_a_step_of_the_change_it_orders() {
    const SESSIONS: u64 = 100;
    let mut measured = Vec::new();
    for mutant in [false, true] {
        let dir = scratch(&format!("need-drop-flushes-{mutant}"));
        let (mut n, fired) = appending(&dir, 1, "");
        let sel = Sel::Need(None);
        n.cfg.on = vec![sel.clone()];
        n.cfg.window_ms = 3_600_000;
        n.cursor_every = Duration::MAX;
        n.cursor_file = mutant;
        n.take(Notice::Log(0, strict_log())).expect("log");
        let face = |i: u64| format!("/f/f1/pub/n-a/{}/ev", generated_sid(i));
        let mut off = 0;
        let mut feed = |n: &mut Notifier, i: u64| {
            let mut s = Needs::open();
            let cmd = s.cmd_open();
            s.cmd_close(cmd, aterm_slog::RunOut::Fail);
            for body in s.bodies() {
                off += 1;
                n.take(Notice::Record(0, off, face(i), body.into_bytes()))
                    .expect("a record");
            }
        };
        feed(&mut n, 0);
        let (dropped, standing) = (n.counts.dropped, n.standing.len());
        let recording = aterm_durable::seam::Recording::start();
        for i in 1..=SESSIONS {
            feed(&mut n, i);
        }
        let flushes = recording.take();
        assert_eq!(
            (
                fired_offsets(&fired).len(),
                n.standing.len() - standing,
                n.counts.dropped - dropped >= SESSIONS
            ),
            (1, usize::try_from(SESSIONS).expect("small"), true),
            "PRECONDITION ({mutant}): a budget of 1 pages the first failure, and each other \
             session's needs are dropped and owed on its own face"
        );
        let full = flushes.iter().filter(|f| f.is_durable()).count();
        let per = (flushes.len() - full) / usize::try_from(SESSIONS).expect("small");
        eprintln!(
            "distinct need drops (the cursor as a file={mutant}): {SESSIONS} sessions ask for \
             {} ordered flushes and {full} full ones, {per} ordered a session",
            flushes.len() - full
        );
        measured.push(per);
        assert_eq!(full, 0, "({mutant}) no drop asks for a full flush");
        if !mutant {
            let cursor = n.cursor(&sel.key());
            assert!(
                cursor.is_some_and(|c| c > 1),
                "PRECONDITION: a cursor: {cursor:?}"
            );
            let cfg = n.cfg.clone();
            drop(n);
            assert_eq!(
                Notifier::new(cfg).expect("reopen").cursor(&sel.key()),
                cursor,
                "the cursor reads back as the notifier wrote it"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(
        measured[0] <= NEED_DROP_FLUSHES_MAX,
        "{} ordered flushes a session, past {NEED_DROP_FLUSHES_MAX}",
        measured[0]
    );
    assert!(
        measured[1] > NEED_DROP_FLUSHES_MAX,
        "NEGATIVE CONTROL: the cursor as a file asks for {} ordered flushes a session, within \
         {NEED_DROP_FLUSHES_MAX}",
        measured[1]
    );
}
