// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHERE A TAPE IS KEPT: the store a HOSTED run's tapes go to, each made for its run and handed to its recorder.

use std::io::Write;

mod custody;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use super::codec::End;
use super::core::TapeCore;
use super::ctl::TapedCtl;
use super::frame::TapeWriter;
use super::header::{Entry, Header};
use crate::supervise::fx::Fx;
use crate::supervise::run::{Ctl, Session, SessionSetup, SuperviseOpts};

/// A tape's id: sixteen hex digits from this process and the moment the run starts, written in its header.
#[must_use]
pub fn tape_id() -> String {
    let ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{:016x}",
        aterm_zstd::xxh64::xxh64(u64::from(std::process::id()), &ns.to_le_bytes())
    )
}

/// The tape file at `path`, made new and `0600`.
/// # Errors
/// The path is taken, or the file cannot be made there.
pub fn create(path: &Path) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

/// The one way this module takes a lock: [`held::HeldTape`]'s fields are
/// private to `held`, so its `hold` is the module's only `try_lock` and the
/// only way to hold what it takes.
mod held {
    use super::super::frame::FrameSink;

    /// A TAPE ITS WRITER HOLDS for as long as it writes: its `flock`, taken exclusively as the file is handed to its writer and let go by `LOCK_UN` as the writer drops it, before the close (clippy.toml's flock release rule: a child forked meanwhile keeps a lock that close alone released).
    pub(super) struct HeldTape {
        file: std::fs::File,
        held: bool,
    }

    impl HeldTape {
        /// `file`, a tape just made, held while it is written.
        pub(super) fn hold(file: std::fs::File) -> Self {
            let file: std::fs::File = file;
            #[cfg(any(target_vendor = "apple", target_os = "linux"))]
            let held = {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the guard's own take: its Drop releases by LOCK_UN"
                )]
                let taken = file.try_lock();
                taken.is_ok()
            };
            #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
            let held = false;
            Self { held, file }
        }
    }

    impl Drop for HeldTape {
        fn drop(&mut self) {
            if self.held {
                let _ = self.file.unlock();
            }
        }
    }

    impl FrameSink for HeldTape {
        fn write_frame(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            self.file.write_frame(bytes)
        }

        fn sync(&mut self) -> std::io::Result<()> {
            self.file.sync()
        }
    }
}

/// Whether `other` names `file`, the tape just made at `path`: the same
/// device and inode where there are such (unix), else the same resolved
/// path.
fn is_file(file: &std::fs::File, path: &Path, other: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = path;
        match (file.metadata(), std::fs::metadata(other)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        match (std::fs::canonicalize(path), std::fs::canonicalize(other)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }
}

/// Run a loop's `body` and say how it ended on its tape: `end` of what it returned, or the message of the panic it unwound with — said, then the unwind goes on.
pub(crate) fn run_taped<T>(fx: &Fx, body: impl FnOnce() -> T, end: impl FnOnce(&T) -> End) -> T {
    if fx.tape().is_none() {
        return body();
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(returned) => {
            fx.finish(end(&returned));
            returned
        }
        Err(panic) => {
            let message = super::replay::panic_message(panic.as_ref());
            fx.finish(End::Unwound { message });
            std::panic::resume_unwind(panic)
        }
    }
}

/// The files a run writes beside its loop, each with what it is: what a tape must not be — its `--journal` and its approval ledger (the setup's, else a hosted loop's `opts.ledger`).
pub(crate) fn beside_files(
    setup: &SessionSetup,
    opts: &SuperviseOpts,
) -> Vec<(&'static str, PathBuf)> {
    let ledger = setup.ledger_path().or(opts.ledger.as_deref());
    [
        ("--journal", opts.journal.clone()),
        ("approval ledger", ledger.map(Path::to_path_buf)),
    ]
    .into_iter()
    .filter_map(|(what, p)| Some((what, p?)))
    .collect()
}

/// THE DEVELOPMENT SEAM that records every hosted run, read by the window's host through
/// `aterm_types::dev_seam!` — so no shipped binary reads it — and set by the lab's instances.
pub const HOSTED_SEAM: &str = "ATERM_HARNESS_TAPE";

/// Whether the seam's `value` asks for tapes: set, and neither empty nor `0`.
#[must_use]
pub fn seam_on(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// The most bytes of hosted tapes the store keeps (the replay plan's §6.5).
pub const KEEP_BYTES: u64 = 256 * 1024 * 1024;

/// The oldest a hosted tape the store keeps may be, by its last write.
pub const KEEP_AGE: Duration = Duration::from_secs(72 * 3600);

/// The store's directory a person keeps tapes in: never swept.
pub const KEPT: &str = "kept";

/// A tape file's extension.
const EXT: &str = "atape";

/// The hosted tapes' store under the state root `state`: `<state>/drive/tapes`.
#[must_use]
pub fn store_under(state: &Path) -> PathBuf {
    state.join("drive").join("tapes")
}

/// A session's directory in the store `store`: the session's name as its approval ledger spells it (`s-1` for `@s-1`, `self` for none: `approvals::ledger_under`), so a session's tapes sit by the name its ledger does.
#[must_use]
pub fn session_dir(store: &Path, sid: Option<&str>) -> PathBuf {
    let ledger = crate::supervise::approvals::ledger_under(Path::new(""), sid);
    let name = ledger
        .file_stem()
        .map_or_else(|| "self".into(), |s| s.to_string_lossy().into_owned());
    store.join(name)
}

/// A hosted tape's file name: `<yyyymmddThhmmss>-<run8>.atape`, the run's start in UTC and the first eight digits of its tape's id — names that sort as the runs began.
#[must_use]
pub fn hosted_name(at: SystemTime, tape: &str) -> String {
    let secs = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let secs = i64::try_from(secs).unwrap_or(i64::MAX);
    let (y, m, d) = aterm_types::rfc3339::civil_from_days(secs.div_euclid(86_400));
    let day = secs.rem_euclid(86_400);
    let run: String = tape.chars().take(8).collect();
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}-{run}.{EXT}",
        day / 3600,
        day % 3600 / 60,
        day % 60
    )
}

/// One tape in the hosted store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    /// Where it is.
    pub path: PathBuf,
    /// Its size.
    pub bytes: u64,
    /// Its last write.
    pub modified: SystemTime,
    /// It is in [`KEPT`]: a person's, never swept.
    pub kept: bool,
}

/// Every admitted tape in the held private store, newest first.
#[must_use]
pub fn list(store: &Path, sid: Option<&str>) -> Vec<Stored> {
    try_list(store, sid).unwrap_or_default()
}

/// List the hosted store without following any directory or tape link.
/// # Errors
/// Any incomplete scan or non-private/non-owned store entry.
pub fn try_list(store: &Path, sid: Option<&str>) -> std::io::Result<Vec<Stored>> {
    let store = custody::Store::open(store, false)?;
    Ok(store.tapes(sid)?.into_iter().map(|t| t.stored).collect())
}

/// Apply the hosted-store retention to admitted tapes.
pub fn sweep(store: &Path, now: SystemTime) -> Vec<PathBuf> {
    sweep_within(store, now, KEEP_BYTES, KEEP_AGE)
}

fn sweep_within(store: &Path, now: SystemTime, bytes: u64, age: Duration) -> Vec<PathBuf> {
    let Ok(store) = custody::Store::open(store, false) else {
        return Vec::new();
    };
    sweep_held(&store, now, bytes, age).unwrap_or_default()
}

fn sweep_held(
    store: &custody::Store,
    now: SystemTime,
    bytes: u64,
    age: Duration,
) -> std::io::Result<Vec<PathBuf>> {
    sweep_held_probing(store, now, bytes, age, custody::Tape::owner)
}

fn sweep_held_probing(
    store: &custody::Store,
    now: SystemTime,
    bytes: u64,
    age: Duration,
    mut probe: impl FnMut(&custody::Tape) -> aterm_uds::ownerlock::Owner,
) -> std::io::Result<Vec<PathBuf>> {
    let mut held = 0u64;
    let mut gone: Vec<custody::Tape> = store
        .tapes(None)?
        .into_iter()
        .filter(|t| !t.stored.kept)
        .filter(|t| {
            let young = now
                .duration_since(t.stored.modified)
                .map_or(true, |since| since <= age);
            held = held.saturating_add(t.stored.bytes);
            !(young && held <= bytes)
        })
        .filter(|t| {
            let owner = probe(t);
            if cfg!(any(target_vendor = "apple", target_os = "linux")) {
                owner == aterm_uds::ownerlock::Owner::Dead
            } else {
                // No writer-hold lane on these platforms, as before this fix.
                owner != aterm_uds::ownerlock::Owner::Live
            }
        })
        .collect();
    gone.reverse();
    Ok(gone
        .into_iter()
        .filter(|t| store.retire(t).is_ok())
        .map(|t| t.stored.path)
        .collect())
}

/// Forget only paths in a complete admitted store inventory.
/// # Errors
/// Admission, inventory, argument, or retirement failure.
pub fn forget(store: &Path, paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let store = custody::Store::open(store, false).map_err(|e| e.to_string())?;
    let held = store.tapes(None).map_err(|e| e.to_string())?;
    let mut doomed = Vec::new();
    for path in paths {
        let found = held
            .iter()
            .position(|t| t.stored.path == *path)
            .ok_or_else(|| {
                format!(
                    "{} is no tape in this store (`tape ls` lists exact paths)",
                    path.display()
                )
            })?;
        if !doomed.contains(&found) {
            doomed.push(found);
        }
    }
    let mut gone = Vec::new();
    for i in doomed {
        store.retire(&held[i]).map_err(|e| e.to_string())?;
        gone.push(held[i].stored.path.clone());
    }
    Ok(gone)
}

/// The door a hosted run reads its world through, recording to a new tape
/// in the store beside its approval ledger — after the
/// store's retention has run — or the shipped live door, said to `warn`,
/// when the run keeps no ledger or its tape cannot be made.
fn hosted_fx(
    setup: &SessionSetup,
    opts: &SuperviseOpts,
    mut warn: Box<dyn Write + Send>,
    now: SystemTime,
) -> Fx {
    let Some(ledger) = setup.ledger_path().or(opts.ledger.as_deref()) else {
        let _ = writeln!(
            warn,
            "aterm harness: tape: the loop keeps no approval ledger, so its tapes have no \
             store — the loop goes on without one"
        );
        return Fx::os();
    };
    let (Some(drive), Some(state)) = (ledger.parent(), ledger.parent().and_then(Path::parent))
    else {
        return Fx::os();
    };
    let store = drive.join("tapes");
    let store = match custody::Store::open(&store, true) {
        Ok(store) => store,
        Err(e) => {
            let _ = writeln!(
                warn,
                "aterm harness: tape store refused: {e} — the loop goes on without one"
            );
            return Fx::os();
        }
    };
    if let Err(e) = sweep_held(&store, now, KEEP_BYTES, KEEP_AGE) {
        let _ = writeln!(
            warn,
            "aterm harness: tape inventory refused: {e} — the loop goes on without one"
        );
        return Fx::os();
    }
    let tape = tape_id();
    let header = Header::capture(tape.clone(), Entry::Hosted, Some(state), setup, opts);
    let beside = beside_files(setup, opts);
    let beside: Vec<(&str, &Path)> = beside.iter().map(|(w, p)| (*w, p.as_path())).collect();
    let made = match store.create(setup.sid(), &hosted_name(now, &tape)) {
        Ok(made) => made,
        Err(e) => {
            let _ = writeln!(
                warn,
                "aterm harness: tape cannot be made: {e} — the loop goes on without one"
            );
            return Fx::os();
        }
    };
    if let Some((what, _)) = beside
        .iter()
        .find(|(_, other)| is_file(&made.file, &made.stored.path, other))
    {
        let cleanup = store.retire(&made);
        let _ = writeln!(
            warn,
            "aterm harness: tape is the run's {what} too; cleanup {cleanup:?} — the loop goes on without one"
        );
        return Fx::os();
    }
    let writer = TapeWriter::start(
        Box::new(held::HeldTape::hold(made.file)),
        made.stored.path.display().to_string(),
        &header.encode(),
        warn,
    );
    let fx = Fx::os();
    let (origin, _) = fx.peek();
    fx.recording(TapeCore::start(
        writer,
        header.roots.clone(),
        origin,
        &header.tape,
    ))
}

/// THE WINDOW'S HOSTED LOOP, RECORDED ([module docs](self)): `setup`'s loop over the transport `ctl` under `opts` (`Session::run_hosted`, `stop` its stop), every value it observes taped — its control lane through the recording door, its window host through the taped host (`Fx::host_door`) — to a new tape in the store beside its ledger.
/// # Errors
/// What `Session::run_hosted` returned.
pub fn run_hosted_taped<C: Ctl>(
    ctl: &mut C,
    setup: SessionSetup,
    opts: &SuperviseOpts,
    stop: Arc<AtomicBool>,
    out: &mut (dyn Write + Send),
    warn: Box<dyn Write + Send>,
    suspended: impl FnOnce() -> bool,
) -> Result<(), String> {
    let fx = hosted_fx(&setup, opts, warn, SystemTime::now());
    let mut taped = TapedCtl::new(ctl, fx.clone(), super::doors::CTL);
    let mut session = Session::from_setup(&mut taped, setup);
    session.set_fx(fx.clone());
    run_taped(
        &fx,
        || session.run_hosted(opts, stop, out),
        |end| match end {
            Ok(()) if suspended() => End::Suspended,
            end => End::Returned {
                how: super::replay::hosted_end(end),
            },
        },
    )
}

#[cfg(test)]
mod tests {
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
}

#[cfg(all(
    test,
    any(target_vendor = "apple", target_os = "linux", target_os = "android")
))]
mod custody_tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt as _, symlink};

    pub(super) fn private_dir(path: &Path) {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .expect("owned directory");
    }

    fn owned_tape(path: &Path, bytes: &[u8]) {
        let mut file = create(path).expect("exclusive owned file");
        file.write_all(bytes).expect("owned bytes");
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1))
            .expect("fixed mtime");
    }

    #[test]
    fn a_store_refuses_linked_or_nonprivate_roots_before_removing_any_tape() {
        let guard = aterm_tempfile::tempdir().expect("exclusive fixture");
        let base = guard
            .path()
            .canonicalize()
            .expect("owned canonical fixture");
        let store = base.join("ordinary");
        private_dir(&store);
        private_dir(&store.join("s-1"));
        let path = store.join("s-1/a.atape");
        owned_tape(&path, b"ordinary");
        assert_eq!(list(&store, None).len(), 1);
        assert_eq!(
            forget(&store, std::slice::from_ref(&path)).expect("ordinary forget"),
            [path]
        );
        owned_tape(&store.join("s-1/b.atape"), b"ordinary sweep");
        assert_eq!(
            sweep_within(
                &store,
                SystemTime::UNIX_EPOCH + Duration::from_secs(10),
                0,
                Duration::ZERO
            )
            .len(),
            1
        );
        let unrelated = store.join("s-1/notes.txt");
        owned_tape(&unrelated, b"not a tape");
        assert!(
            sweep_within(
                &store,
                SystemTime::UNIX_EPOCH + Duration::from_secs(10),
                0,
                Duration::ZERO
            )
            .is_empty()
        );
        assert_eq!(
            std::fs::read(&unrelated).expect("unrelated retained"),
            b"not a tape"
        );

        let outside = base.join("outside");
        private_dir(&outside);
        private_dir(&outside.join("s-1"));
        let target = outside.join("s-1/foreign.atape");
        let alias = base.join("linked-store");
        symlink(&outside, &alias).expect("owned fixture link");
        owned_tape(&target, b"must survive forget");
        let linked_list_refused = list(&alias, None).is_empty();
        let linked_forget_refused = forget(&alias, &[alias.join("s-1/foreign.atape")]).is_err();
        let survived_forget = std::fs::read(&target).is_ok_and(|b| b == b"must survive forget");
        if !target.exists() {
            owned_tape(&target, b"must survive sweep");
        }
        let before_sweep = std::fs::read(&target).expect("owned sweep fixture");
        let swept = sweep_within(
            &alias,
            SystemTime::UNIX_EPOCH + Duration::from_secs(10),
            0,
            Duration::ZERO,
        );
        let survived_sweep = std::fs::read(&target).is_ok_and(|bytes| bytes == before_sweep);

        let nonprivate = base.join("nonprivate");
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(&nonprivate)
            .expect("owned nonprivate directory");
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&nonprivate, std::fs::Permissions::from_mode(0o755))
            .expect("owned nonprivate mode");
        private_dir(&nonprivate.join("s-1"));
        let occupied = nonprivate.join("s-1/occupied.atape");
        owned_tape(&occupied, b"occupied");
        let nonprivate_refused = forget(&nonprivate, std::slice::from_ref(&occupied)).is_err();
        let nonprivate_preserved = std::fs::read(&occupied).is_ok_and(|b| b == b"occupied");
        assert!(
            linked_list_refused
                && linked_forget_refused
                && survived_forget
                && swept.is_empty()
                && survived_sweep
                && nonprivate_refused
                && nonprivate_preserved,
            "hosted tape cleanup must refuse linked or nonprivate roots and preserve unrelated bytes"
        );
    }
}

#[cfg(all(test, any(target_vendor = "apple", target_os = "linux")))]
mod unknown_hold_tests {
    use super::*;
    use aterm_uds::ownerlock::Owner;

    fn tape(path: &Path, bytes: &[u8]) {
        let mut file = create(path).expect("exclusive owned tape");
        file.write_all(bytes).expect("owned bytes");
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1))
            .expect("fixed mtime");
    }

    #[test]
    fn an_unknown_tape_hold_does_not_authorize_retention_removal() {
        let guard = aterm_tempfile::tempdir().expect("exclusive fixture");
        let root = guard
            .path()
            .canonicalize()
            .expect("owned canonical fixture")
            .join("tapes");
        custody_tests::private_dir(&root);
        custody_tests::private_dir(&root.join("s-1"));
        let store = custody::Store::open(&root, false).expect("admitted held store");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        let dead = root.join("s-1/dead.atape");
        tape(&dead, b"free");
        let removed = sweep_held_probing(&store, now, 0, Duration::ZERO, |_| Owner::Dead)
            .expect("ordinary free lock");
        assert_eq!(removed.as_slice(), std::slice::from_ref(&dead));
        assert!(!dead.exists());
        let live = root.join("s-1/live.atape");
        tape(&live, b"held");
        assert!(
            sweep_held_probing(&store, now, 0, Duration::ZERO, |_| Owner::Live)
                .expect("ordinary held lock")
                .is_empty()
        );
        assert_eq!(std::fs::read(&live).expect("live retained"), b"held");
        let unknown = root.join("s-1/unknown.atape");
        tape(&unknown, b"no observation");
        let mut asked = Vec::new();
        let gone = sweep_held_probing(&store, now, 0, Duration::ZERO, |t| {
            asked.push(t.stored.path.clone());
            if t.stored.path == live {
                Owner::Live
            } else {
                Owner::Unknown
            }
        })
        .expect("complete inventory");
        assert!(asked.contains(&live) && asked.contains(&unknown));
        assert!(
            gone.is_empty() && std::fs::read(&unknown).is_ok_and(|b| b == b"no observation"),
            "an unknown tape hold must not authorize retention removal"
        );
    }
}
