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

/// A tape's id: sixteen hex digits from this process, a monotonic sequence, and the moment the run starts, written in its header.
#[must_use]
pub fn tape_id() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut input = [0u8; 24];
    input[..16].copy_from_slice(&ns.to_le_bytes());
    input[16..].copy_from_slice(&seq.to_le_bytes());
    format!(
        "{:016x}",
        aterm_zstd::xxh64::xxh64(u64::from(std::process::id()), &input)
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
        o.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    aterm_uds::kept_out(|| o.open(path))
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
            #[cfg(unix)]
            let _ = aterm_uds::keep_out_of_children(&file);
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
/// store's retention has run — or the setup's live door
/// ([`SessionSetup::live_fx`]), said to `warn`, when the run keeps no ledger
/// or its tape cannot be made.
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
        return setup.live_fx();
    };
    let (Some(drive), Some(state)) = (ledger.parent(), ledger.parent().and_then(Path::parent))
    else {
        return setup.live_fx();
    };
    let store = drive.join("tapes");
    let store = match custody::Store::open(&store, true) {
        Ok(store) => store,
        Err(e) => {
            let _ = writeln!(
                warn,
                "aterm harness: tape store refused: {e} — the loop goes on without one"
            );
            return setup.live_fx();
        }
    };
    if let Err(e) = sweep_held(&store, now, KEEP_BYTES, KEEP_AGE) {
        let _ = writeln!(
            warn,
            "aterm harness: tape inventory refused: {e} — the loop goes on without one"
        );
        return setup.live_fx();
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
            return setup.live_fx();
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
        return setup.live_fx();
    }
    let writer = TapeWriter::start(
        Box::new(held::HeldTape::hold(made.file)),
        made.stored.path.display().to_string(),
        &header.encode(),
        warn,
    );
    let fx = setup.live_fx();
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
#[path = "store_tests.rs"]
mod tests;

#[cfg(all(
    test,
    any(target_vendor = "apple", target_os = "linux", target_os = "android")
))]
#[path = "store_custody_tests.rs"]
mod custody_tests;

#[cfg(all(test, any(target_vendor = "apple", target_os = "linux")))]
#[path = "store_unknown_hold_tests.rs"]
mod unknown_hold_tests;
