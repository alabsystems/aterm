// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A tape is made `0600`, and never over a file already at its path.
#[test]
fn a_tape_is_made_new_and_private() {
    let dir = crate::supervise::ScratchDir::new("tape-store", "private");
    let path = dir.join("run.atape");
    let file = create(&path).expect("made");
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
    }
    std::fs::write(&path, b"an earlier run's tape").expect("write");
    let again = create(&path).expect_err("taken");
    assert_eq!(again.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read(&path).expect("read"),
        b"an earlier run's tape",
        "never written over"
    );
}

/// A file of `bytes` bytes at `path`, written `ago` before `now`.
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
fn tape_at(path: &Path, bytes: usize, now: SystemTime, ago: Duration) {
    let mut made = create(path).expect("new owned tape");
    made.write_all(&vec![0u8; bytes]).expect("write");
    drop(made);
    let file = std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open");
    file.set_modified(now - ago).expect("mtime");
}

/// A HOSTED RUN'S DOOR READS ITS SETUP'S HOST: recording to a new tape, or live when the run
/// keeps no ledger, the door of a setup whose host never sleeps
/// (`Session::set_host_awake`) reads that host's full wake, and a setup without it the
/// system's, of which a test build's power manager says nothing.
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
#[test]
fn a_hosted_runs_door_reads_its_setups_host() {
    struct Unasked;
    impl Ctl for Unasked {
        fn call(&mut self, args: &[&str]) -> Result<crate::supervise::run::CtlReply, String> {
            panic!("a door test sent {args:?}");
        }
    }
    let guard = crate::supervise::ScratchDir::new("tape-store", "awake");
    let state = guard.canonicalize().expect("owned canonical fixture");
    let ledger =
        crate::supervise::approvals::path_under(&state, Some("@s-1")).expect("the ledger's drive/");
    let taped = SuperviseOpts {
        ledger: Some(ledger),
        ..SuperviseOpts::hosted()
    };
    let untaped = SuperviseOpts::hosted();
    for awake in [false, true] {
        let mut ctl = Unasked;
        let mut session = Session::new(&mut ctl, Some("@s-1".to_string()));
        if awake {
            session.set_host_awake();
        }
        let setup = session.setup().clone();
        for opts in [&taped, &untaped] {
            let fx = hosted_fx(&setup, opts, Box::new(std::io::sink()), SystemTime::now());
            let recorded = opts.ledger.is_some();
            assert_eq!(fx.tape().is_some(), recorded);
            assert_eq!(
                fx.full_wake(),
                awake.then_some(true),
                "awake {awake}, recorded {recorded}"
            );
            fx.finish(End::Suspended);
        }
    }
}

/// A hosted tape's name is the run's start in UTC and its tape's first
/// eight digits, so names sort as the runs began.
#[test]
fn a_hosted_tapes_name_sorts_as_the_runs_began() {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_789_660_500);
    assert_eq!(
        hosted_name(at, "0123456789abcdef"),
        "20260917T155500-01234567.atape"
    );
    let later = hosted_name(at + Duration::from_secs(1), "ffffffff00000000");
    assert!(hosted_name(at, "0123456789abcdef") < later);
    assert_eq!(
        session_dir(Path::new("/st/drive/tapes"), Some("@s-1")),
        Path::new("/st/drive/tapes/s-1")
    );
    assert_eq!(
        session_dir(Path::new("/st/drive/tapes"), None),
        Path::new("/st/drive/tapes/self")
    );
    assert!(seam_on(Some(std::ffi::OsStr::new("1"))));
    assert!(!seam_on(Some(std::ffi::OsStr::new("0"))));
    assert!(!seam_on(Some(std::ffi::OsStr::new(""))));
    assert!(!seam_on(None));
    let first_id = tape_id();
    let second_id = tape_id();
    assert_eq!(first_id.len(), 16);
    assert_ne!(
        first_id, second_id,
        "consecutive tape_id calls never collide"
    );
}

/// THE STORE'S RETENTION: the sessions' tapes, newest first, are kept
/// within the bytes and the age, and every other is removed — by size
/// (the oldest that no longer fits, with every older one) and by age (a
/// tape past it, however small); a kept tape is neither removed nor
/// counted, and a file that is no tape is left alone.
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
#[test]
fn the_store_keeps_the_newest_within_its_bytes_and_age() {
    let guard = crate::supervise::ScratchDir::new("tape-store", "sweep");
    let store = guard.canonicalize().expect("owned canonical fixture");
    let (a, b) = (store.join("s-1"), store.join("s-2"));
    for dir in [&a, &b, &store.join(KEPT)] {
        custody_tests::private_dir(dir);
    }
    let now = SystemTime::now();
    let min = Duration::from_secs(60);
    tape_at(&a.join("1.atape"), 400, now, min);
    tape_at(&b.join("2.atape"), 400, now, 2 * min);
    tape_at(&a.join("3.atape"), 400, now, 3 * min);
    tape_at(&b.join("4.atape"), 10, now, 4 * min);
    tape_at(&a.join("old.atape"), 10, now, 100 * min);
    tape_at(&store.join(KEPT).join("k.atape"), 5000, now, 1000 * min);
    tape_at(&a.join("notes.txt"), 5000, now, 1000 * min);
    let gone = sweep_within(&store, now, 1000, 50 * min);
    let name = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().into_owned());
    let gone: Vec<_> = gone.iter().filter_map(name).collect();
    assert_eq!(
        gone,
        ["old.atape", "4.atape", "3.atape"],
        "the third no longer fits, and every older tape goes with it, oldest first"
    );
    let left: Vec<_> = list(&store, None).iter().map(|t| name(&t.path)).collect();
    assert_eq!(
        left,
        [
            Some("1.atape".into()),
            Some("2.atape".into()),
            Some("k.atape".into())
        ]
    );
    assert!(a.join("notes.txt").exists(), "no tape, left alone");
    assert_eq!(
        list(&store, Some("@s-2")).len(),
        2,
        "one session's tapes, and the kept ones beside them"
    );
}

/// A TAPE ITS WRITER STILL HOLDS IS NEVER SWEPT, and its bytes count: a
/// running loop's tape past the store's bytes — the newer tape before it
/// in the walk already near the bound — and another past its age (a
/// capped tape whose loop runs on writes no more) both stay, while the
/// closed tape the held one's bytes push past the bound goes.
#[test]
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn a_tape_its_writer_still_holds_is_never_swept() {
    use std::io::Write as _;
    let guard = crate::supervise::ScratchDir::new("tape-store", "held");
    let store = guard.canonicalize().expect("owned canonical fixture");
    let dir = store.join("s-1");
    custody_tests::private_dir(&dir);
    let now = SystemTime::now();
    let min = Duration::from_secs(60);
    let live = |name: &str, bytes: usize, ago: Duration| {
        let path = dir.join(name);
        let mut file = create(&path).expect("made");
        file.write_all(&vec![0u8; bytes]).expect("write");
        file.set_modified(now - ago).expect("mtime");
        held::HeldTape::hold(file)
    };
    tape_at(&dir.join("a.atape"), 500, now, min);
    let over_bytes = live("over-bytes.atape", 600, 2 * min);
    let over_age = live("over-age.atape", 10, 100 * min);
    tape_at(&dir.join("closed.atape"), 10, now, 3 * min);
    let name = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().into_owned());
    let names = |gone: Vec<PathBuf>| gone.iter().filter_map(name).collect::<Vec<_>>();
    assert_eq!(
        names(sweep_within(&store, now, 1000, 50 * min)),
        ["closed.atape"],
        "the held tapes stay, and the first counts toward the bound"
    );
    assert!(dir.join("over-bytes.atape").exists() && dir.join("over-age.atape").exists());
    drop((over_bytes, over_age));
    assert_eq!(
        names(sweep_within(&store, now, 1000, 50 * min)),
        ["over-age.atape", "over-bytes.atape"],
        "let go, each is swept as any other, oldest first"
    );
    assert_eq!(list(&store, None).len(), 1, "a.atape, within both bounds");
}

/// Forgetting removes tapes in the store and nothing else: a file outside
/// it, or one in it that is no tape, is refused before anything goes.
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
#[test]
fn forgetting_removes_only_tapes_in_the_store() {
    let guard = crate::supervise::ScratchDir::new("tape-store", "forget");
    let store = guard.canonicalize().expect("owned canonical fixture");
    let dir = store.join("s-1");
    custody_tests::private_dir(&dir);
    let now = SystemTime::now();
    tape_at(&dir.join("a.atape"), 1, now, Duration::ZERO);
    tape_at(&dir.join("b.atape"), 1, now, Duration::ZERO);
    tape_at(&dir.join("c.txt"), 1, now, Duration::ZERO);
    let outside = crate::supervise::ScratchDir::new("tape-store", "outside");
    tape_at(&outside.join("x.atape"), 1, now, Duration::ZERO);
    for refused in [
        outside.join("x.atape"),
        dir.join("c.txt"),
        dir.join("nope.atape"),
    ] {
        let err = forget(&store, &[dir.join("a.atape"), refused]).expect_err("refused");
        assert!(err.contains("no tape"), "{err}");
        assert!(
            dir.join("a.atape").exists(),
            "nothing removed before the refusal"
        );
    }
    assert_eq!(
        forget(&store, &[dir.join("a.atape")]).expect("forgotten"),
        [dir.join("a.atape")]
    );
    assert!(!dir.join("a.atape").exists() && dir.join("b.atape").exists());
}
