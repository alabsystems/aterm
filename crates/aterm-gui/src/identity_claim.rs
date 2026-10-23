// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One live holder per session id, machine-wide.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use aterm_session::SessionId;

/// Subdirectory of the per-user control directory holding claim files.
const CLAIMS_DIR: &str = "claims";

/// An exclusive hold on one session id, alive while this value is.
pub(crate) struct Claim {
    /// The locked claim file, held open so the lock stays taken. Unix reads it in
    /// `Drop` to unlock; on Windows the open handle's share mode IS the claim.
    #[cfg_attr(
        windows,
        expect(dead_code, reason = "held for its close: the open handle is the claim")
    )]
    file: std::fs::File,
    /// The id this claim holds — so [`settle_transferred`] can tell an id it already
    /// holds from one it still has to take.
    sid: String,
}

/// The drop releases the claim at once with `LOCK_UN`, not the close: a child
/// another thread is spawning holds a copy of every descriptor until it execs.
impl Drop for Claim {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = self.file.unlock();
    }
}

/// The answer to a claim request.
pub(crate) enum ClaimOutcome {
    /// Held.
    Held(Claim),
    /// Another live process holds this id.
    Taken,
    /// The claim could not be taken or proved: no usable control directory, a malformed id, or an I/O error.
    Unprovable(String),
}

/// Every claim this process holds, kept alive for its whole life. A `Vec`:
/// claims are taken a handful of times per process and never looked up.
static HELD: std::sync::Mutex<Vec<Claim>> = std::sync::Mutex::new(Vec::new());

/// Park a claim for the process's lifetime.
pub(crate) fn hold(claim: Claim) {
    HELD.lock().unwrap_or_else(|p| p.into_inner()).push(claim);
}

/// Drop every claim this process holds.
#[cfg(test)]
pub(crate) fn release_claims_for_test() {
    HELD.lock().unwrap_or_else(|p| p.into_inner()).clear();
}

/// The ids this process adopted (from the environment premint or a handoff)
/// rather than minted: the only ids of ours another process can also hold.
static ADOPTED: std::sync::RwLock<Vec<String>> = std::sync::RwLock::new(Vec::new());

/// Record that this process is answering to an id it did NOT mint.
pub(crate) fn note_adopted(sid: &SessionId) {
    let mut g = ADOPTED.write().unwrap_or_else(|p| p.into_inner());
    if !g.iter().any(|s| s == sid.as_str()) {
        g.push(sid.as_str().to_string());
    }
}

/// Whether `sid` is one this process adopted; see [`ADOPTED`].
fn adopted_here(sid: &SessionId) -> bool {
    let guard = ADOPTED.read();
    let adopted = guard.unwrap_or_else(|p| p.into_inner());
    adopted.iter().any(|s| s == sid.as_str())
}

/// The claim file for `sid` under `dir`, or `None` for an id not of the minted
/// shape.
fn claim_path(dir: &Path, sid: &SessionId) -> Option<PathBuf> {
    crate::spawn::is_valid_session_id(sid.as_str()).then(|| dir.join(CLAIMS_DIR).join(sid.as_str()))
}

/// Take the exclusive claim on `sid` under control directory `dir` (a parameter so tests can use a scratch directory).
pub(crate) fn claim_in(dir: &Path, sid: &SessionId) -> ClaimOutcome {
    let Some(path) = claim_path(dir, sid) else {
        return ClaimOutcome::Unprovable(format!("{} is not a session id", sid.as_str()));
    };
    if let Err(e) = crate::control_auth::ensure_private_dir(&dir.join(CLAIMS_DIR)) {
        return ClaimOutcome::Unprovable(format!("claims directory unusable: {e}"));
    }
    let file = match open_claim(&path) {
        Ok(f) => f,
        Err(e) if is_sharing_violation(&e) => return ClaimOutcome::Taken,
        Err(e) => return ClaimOutcome::Unprovable(format!("{}: {e}", path.display())),
    };
    match lock_exclusive(&file) {
        Ok(true) => {}
        Ok(false) => return ClaimOutcome::Taken,
        Err(e) => return ClaimOutcome::Unprovable(format!("{}: {e}", path.display())),
    }
    let _ = write_pid(&file);
    ClaimOutcome::Held(Claim {
        file,
        sid: sid.as_str().to_string(),
    })
}

/// The successor marker beside a claim file: `claims/<sid>.successor`.
fn successor_marker_path(dir: &Path, sid: &SessionId) -> Option<PathBuf> {
    claim_path(dir, sid).map(|claim| claim.with_file_name(format!("{}.successor", sid.as_str())))
}

/// The epoch record beside a claim file: `claims/<sid>.ep`, the largest epoch a holder of the
/// id gave its log.
fn epoch_record_path(dir: &Path, sid: &SessionId) -> Option<PathBuf> {
    claim_path(dir, sid).map(|claim| claim.with_file_name(format!("{}.ep", sid.as_str())))
}

/// A small claim-side record, read without following or waiting on a final
/// link or FIFO.
#[cfg(any(unix, windows))]
fn read_claim_record(path: &Path) -> Option<String> {
    use std::io::Read as _;
    if !std::fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > 16 {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != crate::control_auth::our_uid() || metadata.nlink() != 1 {
            return None;
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & 0x0000_0400 != 0 {
            return None;
        }
    }
    let mut text = String::new();
    file.take(17).read_to_string(&mut text).ok()?;
    (text.len() <= 16).then_some(text)
}

#[cfg(not(any(unix, windows)))]
fn read_claim_record(_: &Path) -> Option<String> {
    None
}

/// Publish through an exclusive directory of this write's own. No existing
/// staging entry is opened or removed.
fn write_claim_record(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a record without a parent",
        )
    })?;
    let temporary = aterm_tempfile::TempDir::new_in(parent)?;
    let staged = temporary.path().join("record");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&staged)?;
    file.write_all(text.as_bytes())?;
    drop(file);
    std::fs::rename(&staged, path)
}

/// The largest epoch a holder of `sid` recorded for its log under control directory `dir`, or `None`.
pub(crate) fn recorded_epoch_in(dir: &Path, sid: &SessionId) -> Option<u32> {
    let text = read_claim_record(&epoch_record_path(dir, sid)?)?;
    text.trim()
        .parse()
        .ok()
        .and_then(crate::session_log::told_epoch)
}

/// Record that this process gave `sid`'s log the epoch `ep`, under control directory `dir`.
pub(crate) fn record_epoch_in(dir: &Path, sid: &SessionId, ep: u32, create: bool) {
    let Some(path) = epoch_record_path(dir, sid) else {
        return;
    };
    if recorded_epoch_in(dir, sid).is_some_and(|had| had >= ep) || (!create && !path.exists()) {
        return;
    }
    if crate::control_auth::ensure_private_dir(&dir.join(CLAIMS_DIR)).is_err() {
        return;
    }
    let _ = write_claim_record(&path, &format!("{ep}\n"));
}

/// [`record_epoch_in`] on the rendezvous directory.
pub(crate) fn record_epoch(sid: &SessionId, ep: u32, create: bool) {
    if let Some(dir) = rendezvous_dir() {
        record_epoch_in(&dir, sid, ep, create);
    }
}

/// The pid a successor marker names for `sid`, whatever its liveness.
fn marked_successor_in(dir: &Path, sid: &SessionId) -> Option<u32> {
    let text = read_claim_record(&successor_marker_path(dir, sid)?)?;
    text.strip_prefix("pid ")?.trim().parse().ok()
}

/// The live successor a handoff marker names for `sid`.
pub(crate) fn live_successor_in(dir: &Path, sid: &SessionId) -> Option<u32> {
    let pid = marked_successor_in(dir, sid)?;
    (pid != std::process::id() && crate::control_auth::pid_alive(pid)).then_some(pid)
}

/// Predecessor, at Commit, before `_exit`.
#[cfg(unix)]
pub(crate) fn mark_successor_in(dir: &Path, sids: &[SessionId], successor: u32) {
    if crate::control_auth::ensure_private_dir(&dir.join(CLAIMS_DIR)).is_err() {
        return;
    }
    for sid in sids {
        let Some(marker) = successor_marker_path(dir, sid) else {
            continue;
        };
        let _ = write_claim_record(&marker, &format!("pid {successor}\n"));
    }
}

/// [`mark_successor_in`] on the rendezvous directory.
#[cfg(unix)]
pub(crate) fn mark_successor(sids: &[SessionId], successor: u32) {
    if let Some(dir) = rendezvous_dir() {
        mark_successor_in(&dir, sids, successor);
    }
}

/// Predecessor, when a Commit failed and it keeps its sessions: withdraw the markers it wrote for `successor` (only those).
#[cfg(unix)]
pub(crate) fn withdraw_successor_markers(sids: &[SessionId], successor: u32) {
    let Some(dir) = rendezvous_dir() else {
        return;
    };
    for sid in sids {
        if marked_successor_in(&dir, sid) == Some(successor)
            && let Some(marker) = successor_marker_path(&dir, sid)
        {
            let _ = std::fs::remove_file(marker);
        }
    }
}

/// Successor, once its own graph entries are published.
pub(crate) fn settle_transferred_in(dir: &Path, sids: &[SessionId], me: u32) {
    for sid in sids {
        let held = HELD
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .any(|claim| claim.sid == sid.as_str());
        if !held && let ClaimOutcome::Held(claim) = claim_in(dir, sid) {
            hold(claim);
        }
        if marked_successor_in(dir, sid) == Some(me)
            && let Some(marker) = successor_marker_path(dir, sid)
        {
            let _ = std::fs::remove_file(marker);
        }
    }
}

/// [`settle_transferred_in`] for every id this process adopted, on the rendezvous directory, as this process.
pub(crate) fn settle_transferred() {
    let Some(dir) = rendezvous_dir() else {
        return;
    };
    let sids: Vec<SessionId> = {
        let guard = ADOPTED.read();
        let adopted = guard.unwrap_or_else(|p| p.into_inner());
        adopted.iter().map(|s| SessionId::new(s.clone())).collect()
    };
    settle_transferred_in(&dir, &sids, std::process::id());
}

/// The pid of a live instance other than this one that serves `sid`, per its published discovery entry (`graph/<sid>`).
pub(crate) fn live_holder_in(dir: &Path, sid: &SessionId) -> Option<u32> {
    let pid = crate::proxy::graph_entry_host_pid(dir, sid)?;
    (pid != std::process::id() && crate::control_auth::pid_alive(pid)).then_some(pid)
}

/// The per-user control directory, unmodified: the one place every instance
/// contends, whatever `--control-sock` says.
fn rendezvous_dir() -> Option<PathBuf> {
    #[cfg(test)]
    {
        let guard = DIR_OVERRIDE.read();
        if let Some(over) = guard.unwrap_or_else(|p| p.into_inner()).clone() {
            return Some(over);
        }
    }
    aterm_uds::control_socket_dir()
}

/// Test-only stand-in for the per-user control directory; see [`rendezvous_dir`].
#[cfg(test)]
static DIR_OVERRIDE: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

/// Test-only: point [`rendezvous_dir`] at `dir` until the guard drops, under [`rendezvous_test_guard`].
#[cfg(test)]
pub(crate) fn rendezvous_override(dir: &Path) -> RendezvousOverride {
    let serial = rendezvous_test_guard();
    *DIR_OVERRIDE.write().unwrap_or_else(|p| p.into_inner()) = Some(dir.to_path_buf());
    RendezvousOverride { _serial: serial }
}

/// [`rendezvous_override`]'s guard: clears the override, then lets the next test in.
#[cfg(test)]
pub(crate) struct RendezvousOverride {
    _serial: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for RendezvousOverride {
    fn drop(&mut self) {
        *DIR_OVERRIDE.write().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

/// Test-only: serialize every test that redirects [`rendezvous_dir`].
#[cfg(test)]
pub(crate) fn rendezvous_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// The probe behind the `@<sid>` dispatch refusal: the pid of a live other instance that also answers to `sid`, if any.
pub(crate) fn live_holder(sid: &SessionId) -> Option<u32> {
    if !adopted_here(sid) {
        return None;
    }
    let pid = live_holder_in(&rendezvous_dir()?, sid)?;
    (HANDOFF_CANDIDATE.load(Ordering::SeqCst) != pid).then_some(pid)
}

/// The pid of this process's own seamless-update candidate while a handoff
/// decides, `0` for none (`pid_alive(0)` is false).
static HANDOFF_CANDIDATE: AtomicU32 = AtomicU32::new(0);

/// A handoff lane's registration of its candidate, held from the candidate's creation (dial or fork) to the lane's return.
#[cfg(unix)]
#[must_use = "the registration holds only while the value lives"]
pub(crate) struct CandidateRegistration {
    pid: u32,
}

/// Register `pid` (the kernel's answer for this process's update candidate: the fork's child, or `LOCAL_PEERPID` at the rendezvous accept) as the transfer being decided.
#[cfg(unix)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HandoffAddressOwner",
        action = "Launch",
        project = "aterm_gui::identity_claim::project_handoff_address_owner"
    )
)]
pub(crate) fn register_handoff_candidate(pid: u32) -> CandidateRegistration {
    HANDOFF_CANDIDATE.store(pid, Ordering::SeqCst);
    CandidateRegistration { pid }
}

#[cfg(unix)]
impl CandidateRegistration {
    /// The attempt is over: the candidate committed or was proven dead. Clears
    /// only its own pid, never a later attempt's registration.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "HandoffAddressOwner",
            action = "Reject",
            project = "aterm_gui::identity_claim::project_handoff_address_owner"
        )
    )]
    fn release(&self) {
        let _ = HANDOFF_CANDIDATE.compare_exchange(self.pid, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}

#[cfg(unix)]
impl Drop for CandidateRegistration {
    fn drop(&mut self) {
        self.release();
    }
}

/// `HandoffAddressOwner`'s variables read off the real state for `sid` under `dir`, with `candidate` this process's update candidate (`0`: none).
#[cfg(all(test, unix))]
pub(crate) fn project_handoff_address_owner(
    dir: &Path,
    sid: &SessionId,
    candidate: u32,
) -> aterm_spec::interp::State {
    let alive = |pid: u32| crate::control_auth::pid_alive(pid);
    let cand = match candidate {
        0 => 0,
        pid if alive(pid) => 1,
        _ => 2,
    };
    let registered = i64::from(candidate != 0 && registered_handoff_candidate() == candidate);
    let entry = match crate::proxy::graph_entry_host_pid(dir, sid) {
        Some(pid) if candidate != 0 && pid == candidate => 1,
        Some(pid) if pid != std::process::id() && alive(pid) => 2,
        _ => 0,
    };
    aterm_spec::interp::State::from([
        ("cand", cand),
        ("registered", registered),
        ("entry", entry),
        ("pred", 1),
        ("asked", 0),
        ("refused", 0),
        ("steps", 0),
    ])
}

/// Test-only: the pid registered now (`0`: none).
#[cfg(all(test, unix))]
pub(crate) fn registered_handoff_candidate() -> u32 {
    HANDOFF_CANDIDATE.load(Ordering::SeqCst)
}

/// Claim an identity this process is adopting.
pub(crate) fn claim_for_adoption(dir: &Path, sid: &SessionId) -> bool {
    if let Some(pid) = live_holder_in(dir, sid) {
        crate::logging::stderr_line!(
            "aterm: session id {} is already served by a live instance (pid {pid}); \
             minting a fresh identity for this one",
            sid.as_str()
        );
        return false;
    }
    if let Some(pid) = live_successor_in(dir, sid) {
        crate::logging::stderr_line!(
            "aterm: session id {} is being handed to a live update successor (pid {pid}); \
             minting a fresh identity for this one",
            sid.as_str()
        );
        return false;
    }
    match claim_in(dir, sid) {
        ClaimOutcome::Held(c) => {
            hold(c);
            note_adopted(sid);
            true
        }
        ClaimOutcome::Taken => {
            crate::logging::stderr_line!(
                "aterm: session id {} is already claimed by another live instance; \
                 minting a fresh identity for this one",
                sid.as_str()
            );
            false
        }
        ClaimOutcome::Unprovable(why) => {
            crate::logging::stderr_line!(
                "aterm: cannot prove session id {} is unused ({why}); \
                 minting a fresh identity rather than risk a duplicate",
                sid.as_str()
            );
            false
        }
    }
}

/// Take the claim on an identity transferred to us by a seamless-update handoff, if free.
pub(crate) fn hold_transferred(sid: &SessionId) {
    note_adopted(sid);
    let Some(dir) = rendezvous_dir() else {
        return;
    };
    if let ClaimOutcome::Held(c) = claim_in(&dir, sid) {
        hold(c);
    }
}

/// Open (or create) the claim file. Unix: 0600, never truncated — a peer may hold
/// it open and locked, and its bytes are diagnostics either way.
#[cfg(unix)]
fn open_claim(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
}

/// Windows: the share mode is the claim. `share_mode(0)` makes a second open
/// fail outright, and the handle closing on exit releases it.
#[cfg(windows)]
fn open_claim(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
}

/// `flock(LOCK_EX | LOCK_NB)`: `Ok(true)` when the lock is ours, `Ok(false)` when
/// a peer holds it, `Err` otherwise (e.g. no `flock` support), which must read
/// as "cannot prove", never "free".
#[cfg(unix)]
fn lock_exclusive(file: &std::fs::File) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;
    // SAFETY: `file` is a live open file, so its descriptor is valid for the call;
    // `flock` mutates no memory and the descriptor outlives it.
    #[expect(
        clippy::disallowed_methods,
        reason = "the claim's lock: held by a `Claim`, whose Drop releases it by LOCK_UN"
    )]
    let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if taken {
        return Ok(true);
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::EWOULDBLOCK) => Ok(false),
        _ => Err(e),
    }
}

/// Windows has no `flock`; the exclusive share mode in [`open_claim`] already did
/// the work, so a handle in hand IS the lock.
#[cfg(windows)]
fn lock_exclusive(_file: &std::fs::File) -> std::io::Result<bool> {
    Ok(true)
}

/// Whether an open failed because a peer holds the file exclusively (Windows'
/// sharing violation).
#[cfg(windows)]
fn is_sharing_violation(e: &std::io::Error) -> bool {
    /// `ERROR_SHARING_VIOLATION`.
    const SHARING_VIOLATION: i32 = 32;
    e.raw_os_error() == Some(SHARING_VIOLATION)
}

#[cfg(unix)]
const fn is_sharing_violation(_e: &std::io::Error) -> bool {
    false
}

/// Stamp the holder's pid into the claim file (diagnostics only; see [`claim_in`]).
fn write_pid(file: &std::fs::File) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut f = file;
    f.write_all(format!("pid {}\n", std::process::id()).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_session::{LaunchNonce, SessionId};

    fn dir() -> aterm_tempfile::TempDir {
        aterm_tempfile::tempdir().expect("scratch control dir")
    }

    /// A pid that is alive on every host and never this process: 1 (init/launchd)
    /// on Unix; 4 (System) on Windows, where pid 1 does not exist and
    /// `OpenProcess` on 4 fails with ERROR_ACCESS_DENIED, which `pid_alive`
    /// counts as alive (as `kill(pid, 0)`'s `EPERM`).
    const LIVE_FOREIGN_PID: u32 = if cfg!(windows) { 4 } else { 1 };

    /// A discovery entry a live foreign instance would have written.
    fn live_foreign_entry() -> String {
        format!("sock /nonexistent/aterm.sock\nnonce ab\npid {LIVE_FOREIGN_PID}\n")
    }

    /// Two aterms reading the same `$ATERM_SESSION_ID` premint: only one may answer to it.
    #[test]
    fn a_second_claim_on_one_id_is_refused_while_the_first_lives() {
        let d = dir();
        let sid = SessionId::generate();
        let first = match claim_in(d.path(), &sid) {
            ClaimOutcome::Held(c) => c,
            _ => panic!("the first claimant must win"),
        };
        assert!(
            matches!(claim_in(d.path(), &sid), ClaimOutcome::Taken),
            "a second live claimant on one id must be refused, not served"
        );
        drop(first);
        assert!(
            matches!(claim_in(d.path(), &sid), ClaimOutcome::Held(_)),
            "a relaunch after the holder exits must adopt its identity"
        );
    }

    /// A dropped claim is free at once even while a duplicate descriptor on the
    /// same open file description (`try_clone`, as a spawning child holds until
    /// it execs) is still open.
    #[cfg(unix)]
    #[test]
    fn a_dropped_claim_is_free_while_a_copy_of_its_descriptor_lives() {
        let d = dir();
        let sid = SessionId::generate();
        let ClaimOutcome::Held(first) = claim_in(d.path(), &sid) else {
            panic!("the first claimant must win");
        };
        let childs_copy = first.file.try_clone().expect("dup the claim's descriptor");
        drop(first);
        assert!(
            matches!(claim_in(d.path(), &sid), ClaimOutcome::Held(_)),
            "a dropped claim is free while a copy of its descriptor lives"
        );
        drop(childs_copy);
    }

    /// Tier-1 binding for `SessionIdClaim`: each model `Launch` is one real
    /// `claim_for_adoption`, and `holders` must equal the callers told yes, with the id free,
    /// locked by a live holder, and unlocked while a live foreign entry holds it (the handoff
    /// shape, where only the entry gate refuses).
    #[test]
    fn session_id_claim_conformance_real_adoption_projects_onto_model() {
        let _guard = rendezvous_test_guard();
        let d = dir();

        let m = aterm_models_session::session_id_claim_model();
        let mut st = m.init_state();
        let premint = SessionId::generate();

        assert!(
            m.fire("Launch", &mut st),
            "the model admits the first launch"
        );
        let first = claim_for_adoption(d.path(), &premint);
        assert!(first, "and the real first launch adopts the premint");
        assert_eq!(st.get("holders"), Some(&1));

        assert!(m.fire("Launch", &mut st), "a second launch happens");
        let second = claim_for_adoption(d.path(), &premint);
        assert!(
            !second,
            "the real second launch is refused, and mints its own"
        );
        assert_eq!(
            st.get("holders"),
            Some(&1),
            "the model and the code agree: still exactly one holder"
        );
        assert!(m.check_invariant("AtMostOneHolder", &st));

        let mut mh = aterm_models_session::session_id_claim_model();
        for c in &mut mh.consts {
            if c.0 == "Handoff" {
                c.1 = 1;
            }
        }
        let mut st = mh.init_state();
        let handed = SessionId::generate();
        assert!(mh.fire("Launch", &mut st));
        assert!(
            claim_for_adoption(d.path(), &handed),
            "the predecessor adopts"
        );

        release_claims_for_test();
        std::fs::create_dir_all(d.path().join("graph")).expect("graph dir");
        std::fs::write(
            d.path().join("graph").join(handed.as_str()),
            live_foreign_entry(),
        )
        .expect("plant the successor's entry");
        assert!(
            mh.fire("PredecessorExits", &mut st),
            "the model admits the predecessor's exit"
        );
        assert!(
            mh.fire("SuccessorPublishes", &mut st),
            "…and the successor's publish"
        );
        assert_eq!(
            st.get("lock"),
            Some(&0),
            "the successor cannot inherit the flock"
        );
        assert_eq!(
            st.get("entry"),
            Some(&1),
            "its own entry is what holds the id"
        );
        assert_eq!(
            st.get("holders"),
            Some(&1),
            "a transfer is not a second holder"
        );

        assert!(mh.fire("Launch", &mut st));
        let after_handoff = claim_for_adoption(d.path(), &handed);
        assert!(
            !after_handoff,
            "the live entry must refuse this launch even though the lock is free — \
             this is the arm a lock-only gate gets wrong"
        );
        assert_eq!(st.get("holders"), Some(&1));
        assert!(mh.check_invariant("AtMostOneHolder", &st));
    }

    /// Tier-1 binding for the handoff window (`SessionIdClaim`'s `PredecessorExits` /
    /// `SuccessorPublishes`) through the real `claim_for_adoption`, `mark_successor_in` and
    /// `settle_transferred_in`: with the predecessor gone (no flock, its entry naming a dead pid)
    /// and the successor unpublished, only the Commit marker refuses a launch.
    #[cfg(unix)]
    #[test]
    fn the_handoff_window_is_held_by_the_successor_marker() {
        let _guard = rendezvous_test_guard();
        let d = dir();
        let base = aterm_models_session::session_id_claim_model();
        let marked = aterm_spec::interp::with_consts(&base, &[("Handoff", 1)]);
        let unmarked = aterm_spec::interp::with_consts(&base, &[("Handoff", 1), ("Unmarked", 1)]);
        let mut successor = aterm_uds::spawnfd::command("/bin/sleep")
            .arg("30")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn the successor stand-in");
        let succ = successor.id();
        let dead_predecessor_entry = |sid: &SessionId| {
            std::fs::create_dir_all(d.path().join("graph")).expect("graph dir");
            std::fs::write(
                d.path().join("graph").join(sid.as_str()),
                "sock /nonexistent/aterm.sock\nnonce ab\npid 2147483646\n",
            )
            .expect("the dead predecessor's entry");
        };

        for (model, write_marker) in [(&marked, true), (&unmarked, false)] {
            let sid = SessionId::generate();
            let mut st = model.init_state();
            assert!(model.fire("Launch", &mut st));
            assert!(claim_for_adoption(d.path(), &sid), "the predecessor adopts");
            if write_marker {
                mark_successor_in(d.path(), std::slice::from_ref(&sid), succ);
            }
            release_claims_for_test();
            dead_predecessor_entry(&sid);
            assert!(model.fire("PredecessorExits", &mut st));
            assert_eq!(
                (st.get("lock"), st.get("entry")),
                (Some(&0), Some(&0)),
                "the window: no lock, no live entry"
            );
            assert!(model.fire("Launch", &mut st));
            let adopted = claim_for_adoption(d.path(), &sid);
            assert_eq!(
                i64::from(adopted) + 1,
                st["holders"],
                "the real launch and the model agree on the holder count \
                 (marker written: {write_marker})"
            );
            if write_marker {
                assert!(!adopted, "the marker refuses the window launch");
                assert!(model.check_invariant("AtMostOneHolder", &st));
            } else {
                assert!(
                    adopted,
                    "NEGATIVE CONTROL: with no marker the real code adopts a second time"
                );
                assert!(!model.check_invariant("AtMostOneHolder", &st));
                release_claims_for_test();
                continue;
            }
            std::fs::write(
                d.path().join("graph").join(sid.as_str()),
                format!("sock /nonexistent/aterm.sock\nnonce ab\npid {succ}\n"),
            )
            .expect("the successor's entry");
            settle_transferred_in(d.path(), std::slice::from_ref(&sid), succ);
            assert_eq!(
                marked_successor_in(d.path(), &sid),
                None,
                "the successor retires its marker"
            );
            assert!(model.fire("SuccessorPublishes", &mut st));
            assert!(model.fire("Launch", &mut st));
            assert!(
                !claim_for_adoption(d.path(), &sid),
                "after the publish, the live entry (and the taken claim) refuse"
            );
            assert_eq!(st["holders"], 1);
            release_claims_for_test();
        }

        let sid = SessionId::generate();
        mark_successor_in(d.path(), std::slice::from_ref(&sid), 2_147_483_646);
        assert_eq!(live_successor_in(d.path(), &sid), None);
        mark_successor_in(d.path(), std::slice::from_ref(&sid), std::process::id());
        assert_eq!(live_successor_in(d.path(), &sid), None);
        assert!(claim_for_adoption(d.path(), &sid));
        release_claims_for_test();

        let _ = successor.kill();
        let _ = successor.wait();
    }

    /// Anything but the minted shape is refused before it becomes a filename,
    /// so a hostile `$ATERM_SESSION_ID` cannot aim the claim at a path; being
    /// `Unprovable`, it also does not adopt.
    #[test]
    fn a_malformed_injected_id_never_becomes_a_path() {
        let d = dir();
        for hostile in [
            "../../escape",
            "s-../../escape",
            "/etc/passwd",
            "s-not-hex-at-all-nope",
            "",
        ] {
            let sid = SessionId::new(hostile);
            assert!(
                claim_path(d.path(), &sid).is_none(),
                "{hostile:?} shaped a path"
            );
            assert!(
                matches!(claim_in(d.path(), &sid), ClaimOutcome::Unprovable(_)),
                "{hostile:?} must be unprovable, never claimable"
            );
            assert!(
                !claim_for_adoption(d.path(), &sid),
                "{hostile:?} must not be adopted"
            );
        }
        assert!(
            !d.path().join(CLAIMS_DIR).join("escape").exists(),
            "no claim file may be created outside the claims directory"
        );
    }

    /// The second gate: an id whose discovery entry names a live foreign process
    /// is held even when its claim file is free; an entry naming a dead process
    /// or us is not.
    #[test]
    fn a_live_discovery_entry_holds_an_id_a_dead_one_does_not() {
        let d = dir();
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();
        assert_eq!(live_holder_in(d.path(), &sid), None, "no entry, no holder");

        crate::proxy::write_graph_entry(d.path(), &sid, "/nonexistent/aterm.sock", &nonce);
        assert_eq!(
            live_holder_in(d.path(), &sid),
            None,
            "an instance must not read its own discovery entry as a rival"
        );

        let foreign = d.path().join("graph").join(sid.as_str());
        std::fs::write(&foreign, live_foreign_entry()).expect("plant a foreign entry");
        assert_eq!(
            live_holder_in(d.path(), &sid),
            Some(LIVE_FOREIGN_PID),
            "a live foreign holder must be reported"
        );
        assert!(
            !claim_for_adoption(d.path(), &sid),
            "an id a live instance publishes must not be adopted, claim file free or not"
        );

        std::fs::write(
            &foreign,
            "sock /nonexistent/aterm.sock\nnonce ab\npid 2147483646\n",
        )
        .expect("plant a dead entry");
        assert_eq!(live_holder_in(d.path(), &sid), None);
        assert!(
            claim_for_adoption(d.path(), &sid),
            "a dead instance's leftover entry must not block a relaunch"
        );
    }

    /// The candidate registration (bound to `HandoffAddressOwner`'s `Launch` and `Reject`):
    /// while it lives, an adopted id whose entry names that pid is served here, and its drop
    /// clears only its own pid.
    #[cfg(unix)]
    #[test]
    fn a_handoff_candidate_registration_is_exactly_its_own() {
        let d = dir();
        let _override = rendezvous_override(d.path());
        let mut successor = aterm_uds::spawnfd::command("/bin/sleep")
            .arg("30")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn the successor stand-in");
        let succ = successor.id();
        let sid = SessionId::generate();
        note_adopted(&sid);
        std::fs::create_dir_all(d.path().join("graph")).expect("graph dir");
        let entry = |pid: u32| {
            std::fs::write(
                d.path().join("graph").join(sid.as_str()),
                format!("sock /nonexistent/aterm-{pid}.sock\nnonce ab\npid {pid}\n"),
            )
            .expect("plant the entry");
        };
        entry(succ);
        assert_eq!(registered_handoff_candidate(), 0, "nothing registered yet");
        assert_eq!(
            live_holder(&sid),
            Some(succ),
            "NEGATIVE CONTROL: unregistered, the early entry is refused (the live line)"
        );

        let reg = register_handoff_candidate(succ);
        assert_eq!(registered_handoff_candidate(), succ);
        assert_eq!(live_holder(&sid), None, "the transfer this process decides");
        assert_eq!(
            live_holder_in(d.path(), &sid),
            Some(succ),
            "adoption still sees the candidate as a holder"
        );
        assert!(
            !claim_for_adoption(d.path(), &sid),
            "a launch may not adopt an id the candidate publishes"
        );
        entry(LIVE_FOREIGN_PID);
        assert_eq!(
            live_holder(&sid),
            Some(LIVE_FOREIGN_PID),
            "NEGATIVE CONTROL: a stranger is refused while the registration lives"
        );

        let later = register_handoff_candidate(LIVE_FOREIGN_PID);
        drop(reg);
        assert_eq!(registered_handoff_candidate(), LIVE_FOREIGN_PID);
        drop(later);
        assert_eq!(
            registered_handoff_candidate(),
            0,
            "every registration dropped"
        );
        entry(succ);
        assert_eq!(
            live_holder(&sid),
            Some(succ),
            "NEGATIVE CONTROL: after the drop the live candidate's entry is refused again"
        );

        let _ = successor.kill();
        let _ = successor.wait();
        assert_eq!(
            live_holder(&sid),
            None,
            "a dead candidate's entry holds nothing"
        );
    }
}

/// `HandoffAddressOwner` environment actions, each waived where its Tier-1 (`tests`)
/// binds it.
#[cfg(any(test, debug_assertions))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HandoffAddressOwner",
    action = "StrangerPublishes",
    reason = "An unrelated instance's discovery entry; the Tier-1 binds plant one naming \
              a live pid that is not this process."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HandoffAddressOwner",
    action = "Commit",
    reason = "`seamless::commit_and_exit` `_exit`s this process: nothing it answers \
              afterwards exists to project."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn handoff_address_owner_scope_waivers() {}
