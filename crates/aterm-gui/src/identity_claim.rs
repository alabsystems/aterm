// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ONE live holder per session id, machine-wide.
//!
//! A session id is minted from 80 bits of OS CSPRNG ([`aterm_session::SessionId::generate`]),
//! so two MINTED ids never collide. Two ADOPTED ones did, and were observed doing
//! so on this machine: a pane's shell exports `ATERM_SESSION_ID` (the identity an
//! outer aterm PREMINTED for the inner aterm that pane may launch, together with
//! its `ATERM_LAUNCH_NONCE` and the three edge tokens), and that export outlives
//! the launch it was minted for. Every aterm started from that shell — or from any
//! descendant of it, an agent CLI included — read the same premint and answered to
//! the same `s-…` id. The premint is deliberately re-readable so a child aterm that
//! exits can be RELAUNCHED in the same shell under its original identity
//! (`proxy::read_edge_tokens` is non-destructive for exactly that reason); what was
//! missing is that a relaunch is a TRANSFER — the previous holder is gone — while a
//! second simultaneous launch is a DUPLICATE.
//!
//! Duplicate ids are not cosmetic. `@<sid>` is the control plane's address: the
//! hosting instance publishes `graph/<sid>` and any client, local or sibling,
//! resolves that one entry. Two live instances answering to one id means a driver
//! that reads a sid out of a fleet listing and sends `@<sid> key enter` can land
//! the keystroke in the OTHER instance's window — a different user's terminal on
//! the same machine. It happened during the investigation that produced this
//! module.
//!
//! ## The claim
//!
//! Before a process may ANSWER to an adopted id it takes an exclusive claim on it:
//! `<control-dir>/claims/<sid>`, held open with `flock(LOCK_EX|LOCK_NB)` (Windows:
//! an exclusive-share-mode handle) for the process's whole life. The kernel is the
//! arbiter, which buys three properties nothing time- or pid-based gives:
//!
//! * **Race-free.** Two aterms launched in the same millisecond from the same
//!   shell contend on one inode; exactly one wins. `flock` locks the open file
//!   DESCRIPTION, not the process, so this holds between two opens inside one
//!   process too — which is what makes it unit-testable without forking.
//! * **Self-releasing.** The lock dies with the holder — exit, crash, `SIGKILL`.
//!   A relaunch in the same shell therefore still adopts (the transfer case), with
//!   no stale-lock sweep to get wrong.
//! * **Nothing to clean up.** Claim files are never unlinked: unlinking one that a
//!   peer already has open and locked is precisely how two processes could both
//!   come to "hold" an id. They are a few dozen bytes, re-used by sid, and inert.
//!
//! ## The second gate, and why one is not enough
//!
//! A SEAMLESS UPDATE hands live sessions to a successor process (`seamless.rs`),
//! which keeps their ids — that is the point, edges and discovery must survive it.
//! The successor is not the process that took the claim and cannot inherit the
//! lock (the claim fd is not in the handoff's proof-carrying fd set, and must not
//! be added to it on a whim), so for the rest of its life it holds an identity
//! whose claim file is free. [`live_holder_in`] closes that: an id whose
//! `graph/<sid>` discovery entry names a LIVE process that is not us is already
//! held, claim or no claim. Adoption requires BOTH gates; a handoff keeps the id
//! unconditionally, since a transfer is not a second holder.
//!
//! ## The third gate: the handoff window
//!
//! A handoff is not one step. For a FIXED-PATH socket (`--control-sock`) the
//! predecessor EXITS (its lock dies with it), and only then does the successor bind and
//! publish its own graph entry — so between the two, neither gate above answered, and a
//! launch landing in exactly that window adopted the id. (A PER-PROCESS socket's
//! successor publishes BEFORE the Commit instead; see the fourth section below.) Closed
//! (2026-09-25) without touching the proof-carrying fd protocol: at
//! Commit, before `_exit`, the predecessor writes `claims/<sid>.successor` naming the
//! attested successor pid for each carried id ([`mark_successor`]), and
//! [`claim_for_adoption`] treats a marker that names a LIVE process other than us as
//! held. Once its entries are published the successor takes each transferred claim —
//! the predecessor is gone, so the lock is free — and retires its markers
//! ([`settle_transferred`]). A stale marker can only name a dead pid (ignored) or a
//! live unrelated one (a safe refusal: the launch mints a fresh identity). The
//! derived model is `SessionIdClaim` (`PredecessorExits` / `SuccessorPublishes`, with
//! `Unmarked = 1` the pre-fix exit that duplicates).
//!
//! ## The other order: a successor that publishes before the Commit
//!
//! A successor on a PER-PROCESS socket (`aterm-<pid>.sock`, every ordinary launch)
//! does not wait for the Commit to bind: two per-process sockets cannot collide, so it
//! binds and publishes `graph/<sid>` for every carried id as soon as it has booted —
//! measured on every update since the in-GUI supervisor host shipped, 191–272 ms
//! BEFORE the predecessor commits. For that window the entry names a live process
//! that is not this one while THIS process still owns the sessions, decides the
//! Commit, and rolls back if it rejects. The `@<sid>` dispatch probe
//! ([`live_holder`]) read that as a second holder and refused the predecessor's own
//! supervisor (`ERR ambiguous session id … also served by pid <successor>`, the
//! 2026-09-28 update to 0.97.0: one supervisor restart, and on a rejected handoff
//! the same refusal counted toward the restart budget).
//!
//! An entry naming THIS process's own kernel-attested update candidate is a
//! transfer in progress, not a second holder: the handoff lane registers the
//! candidate's pid ([`register_handoff_candidate`]) as it makes the candidate — at
//! the dial on the launched lane, before the grant hands it a single descriptor;
//! right after the fork on the fork lane, microseconds before the child could have
//! booted — for as long as the attempt decides, and [`live_holder`] does not refuse an id
//! whose entry names it. Nothing else changes: the pid is the kernel's (the fork,
//! or `LOCAL_PEERPID` at the rendezvous accept), a stranger publishing the same id
//! is refused as before, and ADOPTION ([`live_holder_in`], [`claim_for_adoption`])
//! still reads the candidate's entry as a holder. The derived model is
//! `HandoffAddressOwner` (`Buggy = 1`: the refusal before the exemption).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use aterm_session::SessionId;

/// Subdirectory of the per-user control directory holding claim files.
const CLAIMS_DIR: &str = "claims";

/// An exclusive hold on one session id, alive while this value is.
///
/// The `File` is the whole point: dropping it drops the kernel lock, so the value
/// must be parked ([`hold`]) for as long as the process answers to the id. It is
/// deliberately not `Clone` — two holders is the bug.
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

/// The drop releases the claim at once: `LOCK_UN`, not the close — a child another
/// thread is spawning holds a copy of every descriptor until it execs, and a claim
/// released only by the close stays taken for that long. (Windows: the handle's
/// exclusive share mode is the claim, and its close the release.)
impl Drop for Claim {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = self.file.unlock();
    }
}

/// What asking for a claim answered. Three outcomes, not two: "another live
/// process holds it" and "I could not find out" are different facts, and only the
/// first is a peer. Both are refusals — a caller that cannot PROVE it is the sole
/// holder must mint a fresh identity — but a log line that says which is the
/// difference between a diagnosable machine and a mysterious one.
pub(crate) enum ClaimOutcome {
    /// Held. Park it with [`hold`] to keep it for the process's life.
    Held(Claim),
    /// Another live process holds this id.
    Taken,
    /// The claim could not be taken or proved: no usable control directory, a
    /// malformed id, or an I/O error. Carries the reason for the caller's log.
    Unprovable(String),
}

/// Every claim this process holds, kept alive for its whole life. A `Vec` and not
/// a map: claims are taken a handful of times per process (the root session's
/// adopted identity, plus a handed-off identity per adopted session), never in a
/// hot path, and nothing ever looks one up.
static HELD: std::sync::Mutex<Vec<Claim>> = std::sync::Mutex::new(Vec::new());

/// Park a claim for the process's lifetime.
pub(crate) fn hold(claim: Claim) {
    HELD.lock().unwrap_or_else(|p| p.into_inner()).push(claim);
}

/// Drop every claim this process holds — the only way a TEST can model a
/// holder EXITING, which is the event that releases the `flock` and makes a
/// relaunch (or a seamless-update successor) able to adopt at all. Never
/// compiled into a shipping binary: in production a claim is released by the
/// process dying, which is precisely why nothing unlinks the file.
#[cfg(test)]
pub(crate) fn release_claims_for_test() {
    HELD.lock().unwrap_or_else(|p| p.into_inner()).clear();
}

/// The ids this process ADOPTED rather than minted — from the environment
/// premint or from a handoff — and therefore the only ids of ours another process
/// can also be holding.
///
/// A minted id is 80 bits of OS CSPRNG that never leaves this process except as
/// an address, so nothing else can come to answer to it; an id that arrived from
/// OUTSIDE is the whole population at risk. Keeping the register makes the
/// dispatch probe a set lookup for every ordinary id and a pair of syscalls only
/// for an id that could actually be contested.
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
    // Receiver named ON the acquisition line (the lock-order census resolves a
    // lock's identity from it, and a rustfmt-split chain leaves it receiver-less),
    // and a LEAF hold: nothing is locked while this guard lives.
    let guard = ADOPTED.read();
    let adopted = guard.unwrap_or_else(|p| p.into_inner());
    adopted.iter().any(|s| s == sid.as_str())
}

/// The claim file for `sid` under `dir`, or `None` for an id that is not the
/// minted shape.
///
/// The shape check is load-bearing, not decoration: the id reaching here comes
/// from the ENVIRONMENT (`$ATERM_SESSION_ID`) or from a handoff manifest, and it
/// becomes a FILENAME. `is_valid_session_id` admits only `s-` plus 20 hex digits,
/// so no separator, `..`, or absolute path can survive it and the join cannot
/// leave `dir`.
fn claim_path(dir: &Path, sid: &SessionId) -> Option<PathBuf> {
    crate::spawn::is_valid_session_id(sid.as_str()).then(|| dir.join(CLAIMS_DIR).join(sid.as_str()))
}

/// Take the exclusive claim on `sid` under control directory `dir`.
///
/// `dir` is the caller's rendezvous directory rather than a resolved one so tests
/// can drive the real filesystem path without touching the user's control dir.
pub(crate) fn claim_in(dir: &Path, sid: &SessionId) -> ClaimOutcome {
    let Some(path) = claim_path(dir, sid) else {
        return ClaimOutcome::Unprovable(format!("{} is not a session id", sid.as_str()));
    };
    // 0700, owner-verified — the same gate the sibling `graph/` and `edges/`
    // subdirectories pass through, so a claim cannot be planted by another user.
    if let Err(e) = crate::control_auth::ensure_private_dir(&dir.join(CLAIMS_DIR)) {
        return ClaimOutcome::Unprovable(format!("claims directory unusable: {e}"));
    }
    let file = match open_claim(&path) {
        Ok(f) => f,
        // Windows refuses the OPEN itself when a peer holds the file exclusively;
        // unix refuses the LOCK below. Both are "taken", and neither is an error
        // worth a reason string.
        Err(e) if is_sharing_violation(&e) => return ClaimOutcome::Taken,
        Err(e) => return ClaimOutcome::Unprovable(format!("{}: {e}", path.display())),
    };
    match lock_exclusive(&file) {
        Ok(true) => {}
        Ok(false) => return ClaimOutcome::Taken,
        Err(e) => return ClaimOutcome::Unprovable(format!("{}: {e}", path.display())),
    }
    // Contents are diagnostics only — the LOCK is the claim. A reader must never
    // conclude anything from this pid: it is whatever process last won, which
    // after a crash is a process that no longer exists.
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

/// The pid a successor marker names for `sid`, whatever its liveness.
fn marked_successor_in(dir: &Path, sid: &SessionId) -> Option<u32> {
    let text = std::fs::read_to_string(successor_marker_path(dir, sid)?).ok()?;
    text.strip_prefix("pid ")?.trim().parse().ok()
}

/// The LIVE successor a handoff marker names for `sid` — a live process other than
/// this one that the predecessor handed the id to at Commit. See the module header's
/// third gate. Pid liveness, as for [`live_holder_in`]: a recycled pid reads as live
/// and costs a fresh identity, never a duplicate.
pub(crate) fn live_successor_in(dir: &Path, sid: &SessionId) -> Option<u32> {
    let pid = marked_successor_in(dir, sid)?;
    (pid != std::process::id() && crate::control_auth::pid_alive(pid)).then_some(pid)
}

/// PREDECESSOR, at Commit, before `_exit`: name `successor` as the process about to
/// answer to each of `sids`, so a launch landing between this process's exit and the
/// successor's publish is refused. Written tmp-then-rename, so a reader sees a whole
/// marker or none; failures are silent (the other two gates still stand, and the
/// Commit must not wait on a filesystem).
#[cfg(unix)]
pub(crate) fn mark_successor_in(dir: &Path, sids: &[SessionId], successor: u32) {
    if crate::control_auth::ensure_private_dir(&dir.join(CLAIMS_DIR)).is_err() {
        return;
    }
    for sid in sids {
        let Some(marker) = successor_marker_path(dir, sid) else {
            continue;
        };
        let tmp = marker.with_file_name(format!(
            ".{}.successor.{}.tmp",
            sid.as_str(),
            std::process::id()
        ));
        if std::fs::write(&tmp, format!("pid {successor}\n")).is_ok()
            && std::fs::rename(&tmp, &marker).is_err()
        {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// [`mark_successor_in`] on the rendezvous directory.
#[cfg(unix)]
pub(crate) fn mark_successor(sids: &[SessionId], successor: u32) {
    if let Some(dir) = rendezvous_dir() {
        mark_successor_in(&dir, sids, successor);
    }
}

/// PREDECESSOR, when a Commit FAILED and it keeps its sessions: withdraw the markers
/// it wrote for `successor` (only those — a marker a later handoff wrote is not ours).
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

/// SUCCESSOR, once its own graph entries are published: take the claim on each
/// transferred id it does not hold yet (the predecessor has exited, so its lock is
/// free), and retire every marker that names `me`. Idempotent and cheap: it touches
/// only `sids`, which in production are the ids this process ADOPTED.
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

/// [`settle_transferred_in`] for every id this process adopted, on the rendezvous
/// directory, as this process.
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

/// Whether a session id is already served by a LIVE instance other than this one,
/// per the discovery entry that instance published (`graph/<sid>`), and which pid.
///
/// Pid liveness rather than a socket dial, deliberately: the entry is same-uid
/// writable, and dialing a path out of it is the confused-deputy hop
/// `control_auth::confine_proxy_sock` exists to gate. A recycled pid can therefore
/// read as live — which costs a legitimate adoption a fresh identity and never
/// costs correctness, the direction this must fail in.
pub(crate) fn live_holder_in(dir: &Path, sid: &SessionId) -> Option<u32> {
    let pid = crate::proxy::graph_entry_host_pid(dir, sid)?;
    (pid != std::process::id() && crate::control_auth::pid_alive(pid)).then_some(pid)
}

/// The well-known per-user control directory, unmodified — the ONE place every
/// instance contends, whatever `--control-sock` says about where its own
/// socket lives. Read-only: no `ensure_private_dir` here, so a probe on the
/// dispatch path costs no `mkdir`/`chmod`/`stat`; [`claim_in`] tightens the
/// subdirectory it actually writes into.
fn rendezvous_dir() -> Option<PathBuf> {
    // Under test a scratch directory stands in, so no unit test reads the live
    // machine's rendezvous (production: no override, this is the well-known path).
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

/// Test-only: point [`rendezvous_dir`] at `dir` (or clear it). Hold
/// [`rendezvous_test_guard`] across the whole window — tests run on parallel
/// threads and this is process-global.
#[cfg(test)]
pub(crate) fn set_rendezvous_override(dir: Option<PathBuf>) {
    *DIR_OVERRIDE.write().unwrap_or_else(|p| p.into_inner()) = dir;
}

/// Test-only: serialize every test that redirects [`rendezvous_dir`].
#[cfg(test)]
pub(crate) fn rendezvous_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// The production probe behind the `@<sid>` dispatch refusal: the pid of a live
/// OTHER instance that also answers to `sid`, if there is one.
///
/// Gated on [`ADOPTED`] FIRST, and that gate is what keeps it off the hot path:
/// an id this process minted cannot be held by anyone else, so the answer for it
/// is `None` by construction and costs one lock read and a short string compare —
/// no `stat`, no `read`, no `kill`. Only an id that arrived from outside pays the
/// two syscalls, and only on a request that names it.
///
/// Measured 2026-09-15 (debug lane, M-series macOS, five rounds of 20 000): a
/// minted id costs 45–64 ns; an adopted id ~1.2 µs with no discovery entry,
/// ~10.4–11.3 µs for our own entry, ~11.7–12.7 µs to refuse a live foreign one —
/// against a 4.8–5.7 µs floor for one request+reply over a unix socket. So the
/// gate, not the probe's own cost, is what keeps this off the hot path.
///
/// NOT THIS PROCESS'S OWN UPDATE CANDIDATE: an entry naming the pid a handoff lane
/// registered ([`register_handoff_candidate`]) is the transfer this process is
/// deciding, and the id is still answered HERE until the Commit (module header, "The
/// other order"). Only the dispatch probe exempts it; adoption does not.
pub(crate) fn live_holder(sid: &SessionId) -> Option<u32> {
    if !adopted_here(sid) {
        return None;
    }
    let pid = live_holder_in(&rendezvous_dir()?, sid)?;
    (HANDOFF_CANDIDATE.load(Ordering::SeqCst) != pid).then_some(pid)
}

/// The pid of THIS process's own seamless-update candidate while a handoff attempt
/// decides, `0` for none — never a live holder's pid, since `pid_alive(0)` is false.
/// Set and cleared only through [`CandidateRegistration`].
static HANDOFF_CANDIDATE: AtomicU32 = AtomicU32::new(0);

/// A handoff lane's registration of its candidate, held for as long as the attempt
/// decides: from the candidate's creation (the dial, or the fork) to the lane's
/// return. A Commit `_exit`s this process with it held; every rejection returns only
/// after the candidate is proven dead, and the drop clears it.
// The handoff lanes that register a candidate are unix's alone.
#[cfg(unix)]
#[must_use = "the registration holds only while the value lives"]
pub(crate) struct CandidateRegistration {
    pid: u32,
}

/// Register `pid` — the kernel's answer for this process's update candidate (the
/// fork's child, or `LOCAL_PEERPID` at the rendezvous accept) — as the transfer this
/// process is deciding: an `@<sid>` whose discovery entry names it is served here
/// until the Commit ([`live_holder`]).
#[cfg(unix)]
#[cfg_attr(
    test,
    aterm_spec::refines(
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
    /// The attempt is over: its candidate committed (this process is gone) or was
    /// proven dead. Clears only its OWN pid — a later attempt's registration is not
    /// ours to end.
    #[cfg_attr(
        test,
        aterm_spec::refines(
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

/// `HandoffAddressOwner`'s variables, read off the REAL state for `sid` under
/// `dir`, with `candidate` this process's update candidate (`0`: none yet):
/// `cand` its liveness (1 alive, 2 dead), `registered` whether the lane's
/// registration names it, `entry` whom `graph/<sid>` names (0 this process or
/// no live holder, 1 the candidate, 2 another live process), `pred` 1 (this
/// process is the one asking), and no request answered yet. The Tier-1 binds
/// fire the model's `Request` on it and compare `refused` with the real
/// dispatch probe's answer.
#[cfg(test)]
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
#[cfg(test)]
pub(crate) fn registered_handoff_candidate() -> u32 {
    HANDOFF_CANDIDATE.load(Ordering::SeqCst)
}

/// Claim an identity this process is ADOPTING, and say whether it may.
///
/// `true` only when this process is provably the sole holder: no live instance
/// publishes the id, and the exclusive claim is ours. The claim is parked for the
/// process's life on the way out — a claim released while the id is still answered
/// to would let the next launch duplicate it.
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

/// Take the claim on an identity that was TRANSFERRED to us (a seamless-update
/// handoff), if it happens to be free.
///
/// Never a refusal: the successor keeps the handed-off id whatever this answers —
/// ids must survive an update or every edge, discovery entry and saved address
/// breaks. During an OVERLAP handoff the predecessor is still alive and still
/// holds the claim, so failing to take it is the normal case, not a fault; the
/// successor takes it later, once the predecessor has exited and its own entries
/// are published ([`settle_transferred`]).
pub(crate) fn hold_transferred(sid: &SessionId) {
    // Registered whatever the claim answers: the successor IS answering to an id
    // it did not mint, which is exactly what the dispatch probe must know.
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

/// Windows twin: the SHARE MODE is the claim. `share_mode(0)` opens with no
/// sharing at all, so a second instance's open fails outright (there being no
/// `flock`), and the handle closing on exit releases it exactly as the lock does.
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
/// a peer holds it, `Err` for anything else (a filesystem without `flock`, say —
/// which must read as "cannot prove", never as "free").
#[cfg(unix)]
fn lock_exclusive(file: &std::fs::File) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;
    // SAFETY: `file` is a live open file, so its descriptor is valid for the call;
    // `flock` mutates no memory and the descriptor outlives it.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
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
/// sharing violation). Unix never refuses the open — it refuses the lock.
#[cfg(windows)]
fn is_sharing_violation(e: &std::io::Error) -> bool {
    /// `ERROR_SHARING_VIOLATION`.
    const SHARING_VIOLATION: i32 = 32;
    e.raw_os_error() == Some(SHARING_VIOLATION)
}

#[cfg(unix)]
fn is_sharing_violation(_e: &std::io::Error) -> bool {
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

    /// A pid that is alive on every host and is never this process: 1
    /// (init/launchd) on Unix. On Windows pid 1 is not a process at all —
    /// `OpenProcess` fails with ERROR_INVALID_PARAMETER, so `pid_alive` reads it
    /// dead (measured) — and 4, the System process, is the one every boot has
    /// (its `OpenProcess` fails ERROR_ACCESS_DENIED, which `pid_alive` counts
    /// as alive, as `kill(pid, 0)`'s `EPERM` is).
    const LIVE_FOREIGN_PID: u32 = if cfg!(windows) { 4 } else { 1 };

    /// A discovery entry a live foreign instance would have written.
    fn live_foreign_entry() -> String {
        format!("sock /nonexistent/aterm.sock\nnonce ab\npid {LIVE_FOREIGN_PID}\n")
    }

    /// THE COLLISION, in one process: two aterms launched from the same shell read
    /// the SAME `$ATERM_SESSION_ID` premint, and only one of them may answer to it.
    ///
    /// Same-process is a faithful stand-in for two instances BECAUSE `flock` locks
    /// the open file description rather than the process — two independent opens
    /// contend whether or not they share a pid. (A rewrite onto POSIX `fcntl`
    /// record locks, which ARE per-process, would pass the second claim here and
    /// fail this test — which is the other thing it pins.)
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
        // ...and the refusal lifts when the holder goes away, which is the
        // same-shell RELAUNCH case (a transfer, not a duplicate).
        drop(first);
        assert!(
            matches!(claim_in(d.path(), &sid), ClaimOutcome::Held(_)),
            "a relaunch after the holder exits must adopt its identity"
        );
    }

    /// A dropped claim is free at once, whatever else holds its descriptor: a child
    /// another thread is spawning holds a copy of every descriptor until it execs, and
    /// a claim released only by the close stayed taken for that long, so the relaunch
    /// case above was refused under a loaded suite. A duplicate descriptor on the same
    /// open file description (`try_clone`, which is `dup`) is what that child holds.
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

    /// **TIER-1 BINDING FOR `SessionIdClaim`** — the REAL `claim_for_adoption`,
    /// driven beside the model, decision for decision.
    ///
    /// The model (aterm-spec's `session_id_claim_model`, discharged by
    /// `derived_session_id_claim_proves_catches_and_multiplies` under the
    /// prove/catch/MULTIPLY protocol and machine-checked by Trust `ty`) proves
    /// that at most one live instance answers to an id, and that the two gates
    /// fail differently: a launch blind to every other holder duplicates the id
    /// at every corner, while a launch that consults the lock but not the live
    /// entry duplicates it only across a seamless-update handoff.
    ///
    /// A theorem about a state machine nobody runs is worth nothing, so this is
    /// the projection. Each `Launch` in the model is one real
    /// `claim_for_adoption`, and the model's `holders` must equal the number of
    /// real callers that were told yes — through the three states that matter:
    /// the id free, the id locked by a live holder, and the id whose lock is
    /// FREE while a live foreign entry holds it, which is the handoff shape and
    /// the only place the entry gate is the thing saying no.
    #[test]
    fn session_id_claim_conformance_real_adoption_projects_onto_model() {
        let _guard = rendezvous_test_guard();
        let d = dir();
        set_rendezvous_override(Some(d.path().to_path_buf()));

        // ── arm 1: the ordinary world (Handoff = 0) ──
        let m = aterm_spec::derive::session_id_claim_model();
        let mut st = m.init_state();
        let premint = SessionId::generate();

        assert!(
            m.fire("Launch", &mut st),
            "the model admits the first launch"
        );
        let first = claim_for_adoption(d.path(), &premint);
        assert!(first, "and the real first launch adopts the premint");
        assert_eq!(st.get("holders"), Some(&1));

        // A SECOND SIMULTANEOUS launch from the same shell reads the SAME
        // premint. The model's Launch is still enabled (nothing stops a
        // process from starting) but must not raise `holders`; the real call
        // must answer no.
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

        // ── arm 2: the HANDOFF shape (Handoff = 1), where the lock is free and
        // a live foreign entry is the only thing holding the id ──
        let mut mh = aterm_spec::derive::session_id_claim_model();
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

        // The successor takes the id over and publishes its own entry, and the
        // predecessor's claim goes away with it — a TRANSFER, not a second
        // holder, so `holders` does not move.
        release_claims_for_test();
        // The successor is a DIFFERENT process, so its entry names a different
        // live pid (`LIVE_FOREIGN_PID`, never us). Writing our
        // OWN pid here would prove nothing: an instance must not read its own
        // entry as a rival, which is the rule
        // `a_live_discovery_entry_holds_an_id_a_dead_one_does_not` pins, and a
        // fixture that ignored it would make this arm pass for the wrong
        // reason (measured: it did, until this comment).
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

        // NOW the launch the `Buggy = 1` arm of the model gets wrong: the lock
        // is genuinely free, and only the live entry says no.
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

    /// **TIER-1 BINDING FOR THE HANDOFF WINDOW** — `SessionIdClaim`'s
    /// `PredecessorExits` / `SuccessorPublishes`, driven through the REAL
    /// `claim_for_adoption`, `mark_successor_in` and `settle_transferred_in`.
    ///
    /// The window: the predecessor has exited (its flock is gone, its graph entry
    /// names a dead pid) and the successor has not published yet. Only the marker
    /// the predecessor wrote at Commit can refuse a launch there — and the NEGATIVE
    /// CONTROL removes it and watches the real code adopt the id a second time,
    /// exactly as the model's `Unmarked = 1` arm does.
    #[cfg(unix)]
    #[test]
    fn the_handoff_window_is_held_by_the_successor_marker() {
        let _guard = rendezvous_test_guard();
        let d = dir();
        let base = aterm_spec::derive::session_id_claim_model();
        let marked = aterm_spec::interp::with_consts(&base, &[("Handoff", 1)]);
        let unmarked = aterm_spec::interp::with_consts(&base, &[("Handoff", 1), ("Unmarked", 1)]);
        // The SUCCESSOR is another live process: a stand-in that is never us.
        let mut successor = std::process::Command::new("/bin/sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
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
            // The predecessor adopts the id.
            assert!(model.fire("Launch", &mut st));
            assert!(claim_for_adoption(d.path(), &sid), "the predecessor adopts");
            // Commit: name the successor (or, the pre-fix exit, do not), then exit.
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
            // A launch lands in the window.
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
            // The successor publishes its entry, takes the free claim, and retires
            // the marker that names it.
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

        // A STALE marker is harmless in the adoption direction: one naming a dead pid
        // does not block a relaunch, and one naming US is not a rival.
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

    /// Two ADOPTIONS of one premint "in the same second": the first takes the id,
    /// the second is told no and mints its own. The ids that result differ —
    /// which is the whole property, stated as the fleet sees it.
    ///
    /// The same second is a STATE, constructed, never a stopwatch reading. Nothing
    /// in minting or in the claim reads a clock (a minted id is 80 CSPRNG bits, the
    /// claim a kernel `flock`), so what makes two launches collide is not how close
    /// together they run: it is that both read ONE premint while the first holder
    /// is still LIVE. The test builds exactly that — one premint handed to both,
    /// the first adopter's claim shown held when the second asks. (It used to
    /// require both adoptions inside one wall-clock second, a precondition that
    /// decided nothing about the guard and failed once under the gate's load.)
    ///
    /// NEGATIVE CONTROL: the claim is what refuses. The other two gates are shown
    /// silent (no discovery entry, no successor marker), and once the first holder
    /// is gone the SAME adoption takes the premint — the same-shell relaunch.
    #[test]
    fn two_instances_minted_in_one_second_cannot_collide() {
        // `release_claims_for_test` (the control below, and two sibling tests)
        // drops every claim this process parked: serialize with it, or a sibling
        // could release the first adopter between the two adoptions.
        let _guard = rendezvous_test_guard();
        let d = dir();
        let premint = SessionId::generate();
        let launch = || {
            if claim_for_adoption(d.path(), &premint) {
                premint.clone()
            } else {
                SessionId::generate()
            }
        };

        let first = launch();
        assert_eq!(first, premint, "the first adopter gets the preminted id");
        // The simultaneity, as a fact: the first holder is live when the second
        // launch asks, and the claim is the only gate that could say no.
        assert!(
            matches!(claim_in(d.path(), &premint), ClaimOutcome::Taken),
            "the first adopter still holds its claim"
        );
        assert_eq!(
            live_holder_in(d.path(), &premint),
            None,
            "no discovery entry"
        );
        assert_eq!(
            live_successor_in(d.path(), &premint),
            None,
            "no handoff marker"
        );

        let second = launch();
        assert_ne!(
            second, premint,
            "the second adopter must NOT answer to an id that is already live"
        );
        assert_ne!(first, second, "two live instances, two ids");

        release_claims_for_test();
        assert_eq!(
            launch(),
            premint,
            "NEGATIVE CONTROL: with the first holder gone the same adoption takes the \
             premint, so its live claim was what refused the second"
        );
        release_claims_for_test();
    }

    /// The id becomes a FILENAME. Anything but the minted shape is refused before
    /// it can, so a hostile `$ATERM_SESSION_ID` cannot aim the claim at a path of
    /// its choosing — and, being `Unprovable`, it also does not adopt.
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

    /// The SECOND gate, the one a seamless-update successor needs: an id whose
    /// discovery entry names a live foreign process is held even when its claim
    /// file is free, and an entry naming a dead one (or naming US) is not.
    #[test]
    fn a_live_discovery_entry_holds_an_id_a_dead_one_does_not() {
        let d = dir();
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();
        assert_eq!(live_holder_in(d.path(), &sid), None, "no entry, no holder");

        // OUR OWN entry is not a peer — this is the ordinary case for every
        // session this instance hosts, and reading it as a conflict would refuse
        // every adoption after the first.
        crate::proxy::write_graph_entry(d.path(), &sid, "/nonexistent/aterm.sock", &nonce);
        assert_eq!(
            live_holder_in(d.path(), &sid),
            None,
            "an instance must not read its own discovery entry as a rival"
        );

        // A LIVE foreign pid (`LIVE_FOREIGN_PID`: alive on every host, never us).
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

        // A DEAD pid is a crash leftover, not a holder: the same-shell relaunch
        // case must still adopt.
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

    // `HandoffAddressOwner`'s other actions are no function of this process's
    // dispatch: explicit scope boundaries, not silent coverage holes.
    #[aterm_spec::spec_unmodeled(
        machine = "HandoffAddressOwner",
        action = "SuccessorPublishes",
        reason = "The successor's own `control::publish_discovery`, run in ANOTHER process; \
                  the Tier-1 binds plant the entry it writes (`sock/nonce/pid`)."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "HandoffAddressOwner",
        action = "StrangerPublishes",
        reason = "An unrelated instance's discovery entry; the Tier-1 binds plant one naming \
                  a live pid that is not this process."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "HandoffAddressOwner",
        action = "Commit",
        reason = "`seamless::commit_and_exit` `_exit`s this process: nothing it answers \
                  afterwards exists to project."
    )]
    #[expect(
        dead_code,
        reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it"
    )]
    fn handoff_address_owner_scope_waivers() {}

    /// THE REGISTRATION IS EXACTLY ITS OWN (the handoff lane's
    /// `register_handoff_candidate`, bound to `HandoffAddressOwner`'s `Launch` and
    /// `Reject`): while it lives, an adopted id whose entry names THAT pid is
    /// served here (the transfer this process decides), and its drop clears only
    /// its own pid. NEGATIVE CONTROLS: without a registration the same entry is
    /// refused — the live refusal of 2026-09-28 — and so, registration or not, is
    /// an entry naming any OTHER live process; and ADOPTION still reads the
    /// candidate's entry as a holder.
    #[cfg(unix)]
    #[test]
    fn a_handoff_candidate_registration_is_exactly_its_own() {
        let _guard = rendezvous_test_guard();
        let d = dir();
        set_rendezvous_override(Some(d.path().to_path_buf()));
        let mut successor = std::process::Command::new("/bin/sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
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

        // A later registration (another attempt) is not ended by this one's drop.
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
        set_rendezvous_override(None);
    }
}
