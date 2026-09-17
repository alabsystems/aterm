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
//! Residual, and stated rather than papered over: between a predecessor's exit and
//! its successor publishing its own graph entry, neither gate answers, so a launch
//! landing in exactly that window can still adopt. Closing it needs the claim fd to
//! ride the handoff, which is a change to a proof-carrying protocol and not one to
//! make as a side effect of this fix.

use std::path::{Path, PathBuf};

use aterm_session::SessionId;

/// Subdirectory of the per-user control directory holding claim files.
const CLAIMS_DIR: &str = "claims";

/// An exclusive hold on one session id, alive while this value is.
///
/// The `File` is the whole point: dropping it drops the kernel lock, so the value
/// must be parked ([`hold`]) for as long as the process answers to the id. It is
/// deliberately not `Clone` — two holders is the bug.
pub(crate) struct Claim {
    /// The locked claim file. Never read; held open so the lock stays taken.
    _file: std::fs::File,
    /// The id this claim covers, for diagnostics.
    sid: String,
}

impl Claim {
    /// The id this claim covers.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn sid(&self) -> &str {
        &self.sid
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
        _file: file,
        sid: sid.as_str().to_string(),
    })
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
/// instance contends, whatever `$ATERM_CONTROL_SOCK` says about where its own
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
pub(crate) fn live_holder(sid: &SessionId) -> Option<u32> {
    if !adopted_here(sid) {
        return None;
    }
    live_holder_in(&rendezvous_dir()?, sid)
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
/// holds the claim, so failing to take it is the normal case, not a fault; what
/// the successor publishes for the id is its own `graph/<sid>` entry, and
/// [`live_holder_in`] reads that.
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
mod measure {
    use super::*;
    use aterm_session::{LaunchNonce, SessionId};
    use std::time::Instant;

    /// WHAT THE DISPATCH PROBE COSTS — a measurement harness, never a gate
    /// (`#[ignore]`d: a wall-clock assertion in the suite is a flake, and this
    /// answers a question, it does not defend an invariant). Four arms
    /// interleaved over five rounds, the last of them the hot path.
    ///
    /// `targo --unverified test -p aterm-gui --lib -- --ignored --nocapture probe_cost`
    ///
    /// Measured 2026-09-15, debug lane, M-series macOS, five rounds of 20 000:
    ///
    /// * minted id (the register miss — every ordinary `@<sid>`): **45–64 ns**
    /// * adopted id, no discovery entry: ~1.2 µs (one failed `open`)
    /// * adopted id, our own entry: ~10.4–11.3 µs
    /// * adopted id, a live foreign entry (the refusal): ~11.7–12.7 µs
    ///
    /// For scale, measured the same way in an OPTIMIZED standalone lane: reading
    /// a graph entry is 10.6–15.2 µs and `kill(pid, 0)` under 1 µs, against a
    /// 4.8–5.7 µs floor for one request+reply over a unix socket with no auth,
    /// no parse and no verb body. So the full probe is ~2× the transport floor
    /// and the gated one is ~1% of it — which is why the register gate, not the
    /// probe's own cost, is what keeps this off the hot path.
    #[test]
    #[ignore]
    fn probe_cost() {
        const N: u32 = 20_000;
        let d = aterm_tempfile::tempdir().expect("dir");
        let present = SessionId::generate();
        let absent = SessionId::generate();
        let mine = SessionId::generate();
        crate::proxy::write_graph_entry(
            d.path(),
            &mine,
            "/nonexistent/aterm.sock",
            &LaunchNonce::generate(),
        );
        std::fs::write(
            d.path().join("graph").join(present.as_str()),
            "sock /nonexistent/aterm.sock\nnonce ab\npid 1\n",
        )
        .expect("plant");

        let mut rows = [(0u128, 0u128, 0u128, 0u128); 5];
        for round in &mut rows {
            let t = Instant::now();
            for _ in 0..N {
                std::hint::black_box(live_holder_in(d.path(), &present));
            }
            round.0 = t.elapsed().as_nanos() / u128::from(N);

            let t = Instant::now();
            for _ in 0..N {
                std::hint::black_box(live_holder_in(d.path(), &absent));
            }
            round.1 = t.elapsed().as_nanos() / u128::from(N);

            let t = Instant::now();
            for _ in 0..N {
                std::hint::black_box(live_holder_in(d.path(), &mine));
            }
            round.2 = t.elapsed().as_nanos() / u128::from(N);

            let t = Instant::now();
            for _ in 0..N {
                std::hint::black_box(live_holder(&present));
            }
            round.3 = t.elapsed().as_nanos() / u128::from(N);
        }
        for (i, r) in rows.iter().enumerate() {
            println!(
                "round {i}: foreign-live {} ns | absent {} ns | self {} ns | \
                 minted-id (register miss, the hot path) {} ns",
                r.0, r.1, r.2, r.3
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_session::{LaunchNonce, SessionId};

    fn dir() -> aterm_tempfile::TempDir {
        aterm_tempfile::tempdir().expect("scratch control dir")
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
        assert_eq!(first.sid(), sid.as_str());
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
        // live pid — pid 1 exists on every unix and is never us. Writing our
        // OWN pid here would prove nothing: an instance must not read its own
        // entry as a rival, which is the rule
        // `a_live_discovery_entry_holds_an_id_a_dead_one_does_not` pins, and a
        // fixture that ignored it would make this arm pass for the wrong
        // reason (measured: it did, until this comment).
        std::fs::create_dir_all(d.path().join("graph")).expect("graph dir");
        std::fs::write(
            d.path().join("graph").join(handed.as_str()),
            "sock /nonexistent/aterm.sock\nnonce ab\npid 1\n",
        )
        .expect("plant the successor's entry");
        assert!(
            mh.fire("Handover", &mut st),
            "the model admits the handover"
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

    /// Two ADOPTIONS of one premint in the same process-second: the first takes the
    /// id, the second is told no and mints its own. The ids that result differ —
    /// which is the whole property, stated as the fleet sees it.
    #[test]
    fn two_instances_minted_in_one_second_cannot_collide() {
        let d = dir();
        let premint = SessionId::generate();
        let start = std::time::Instant::now();

        let first = if claim_for_adoption(d.path(), &premint) {
            premint.clone()
        } else {
            SessionId::generate()
        };
        let second = if claim_for_adoption(d.path(), &premint) {
            premint.clone()
        } else {
            SessionId::generate()
        };

        assert_eq!(first, premint, "the first adopter gets the preminted id");
        assert_ne!(
            second, premint,
            "the second adopter must NOT answer to an id that is already live"
        );
        assert_ne!(first, second, "two live instances, two ids");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "both adoptions must land inside one process-second for this to be \
             the same-second case; took {:?}",
            start.elapsed()
        );
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

        // A LIVE foreign pid: pid 1 exists on every unix and is never us.
        let foreign = d.path().join("graph").join(sid.as_str());
        std::fs::write(&foreign, "sock /nonexistent/aterm.sock\nnonce ab\npid 1\n")
            .expect("plant a foreign entry");
        assert_eq!(
            live_holder_in(d.path(), &sid),
            Some(1),
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
}
