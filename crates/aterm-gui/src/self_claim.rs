// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PROVEN SELF-CLAIM (the codex-alab native harness contract, §1–§2): an
//! agent ALab builds from source supervises ITSELF, and aterm's own lane
//! stands down for it — but only where the kernel proves the claim is the
//! agent's own.
//!
//! A `meta set supervisor codex-alab@<pid>` is PROVEN when, and only when:
//!
//! * it arrived on a DIRECT local connection — one a pooled lane accepted on
//!   the instance's own listener, never the bridge's inherited `socketpair`
//!   (nor a `dial`'s takeover, a `@child` proxy or a network relay, whose
//!   peer is a relay process, never the agent);
//! * the kernel's peer pid of that connection, read right after the request
//!   line ([`aterm_uds::fdpass::peer_pid`], Darwin's `LOCAL_PEERPID`: the
//!   connection's last writer), EQUALS `<pid>`;
//! * `<pid>` LEADS the session's foreground process group, read live from
//!   the PTY master (`tcgetpgrp`), not from the sweep's last sighting;
//! * `<pid>` RUNS AN ALAB BUILD: the kernel's image of the process
//!   (`proc_pidpath`) lies in a Codex package whose root carries
//!   `alab-build.json` ([`upgrade_codex::alab_bytes`]), so another program
//!   that leads the group — a shell, a vendor `codex`, a tool a model ran in
//!   the foreground — proves nothing, whatever pid it names;
//! * the claim is connection-bound (no `ttl=`): the connection is
//!   close-on-exec, so an `execve` into another image ends the claim with it;
//! * and the kernel names its start ([`restart_stamp::kernel_start`]), which
//!   the claim keeps as its identity (a reused pid is never the claim's).
//!
//! A descendant proves nothing while it runs another image: a model's tool
//! process is a child of the TUI, its own pid is its peer pid, and its image
//! is never an ALab build's (a tool that is codex-alab itself is the agent
//! all the same). The holder's spelling proves nothing either: an unproven
//! `codex-alab@…` claim is an ordinary foreign claim ([`Proven`] absent).
//!
//! What a proof buys (`session_timeline::claim_supervisor_as`): it REPLACES a
//! live `aterm-harness@…` claim (the window host's own; its loop's connection
//! is cut at once) and a proven claim whose process is gone; any other
//! replacement stays `ERR busy`. A proven claim is renewed only by a proven
//! request, and is cleared when its process leaves the foreground group or
//! exits (`session_timeline::release_proven_if_gone`, from the status sweep).

use std::sync::{Mutex, PoisonError};

use aterm_agent::harness::self_claimed::{self, Copy};
use aterm_agent::harness::{restart_stamp, upgrade_codex};

/// The holder prefix a self-claim is made under: `codex-alab@<pid>`.
pub(crate) const SELF_CLAIM_PREFIX: &str = "codex-alab@";

/// A claim the kernel proved is its agent's own ([`prove`]): the agent's pid
/// and its kernel start, the identity every later look checks it by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Proven {
    /// The agent's pid: the connection's peer and the foreground group's
    /// leader at the claim.
    pub(crate) pid: u32,
    /// Its kernel start, as [`restart_stamp::kernel_start`] spells it.
    pub(crate) start: String,
}

impl Proven {
    /// Whether the very process the proof named still runs (its pid, alive,
    /// with the same kernel start). A syscall on macOS (a `ps` elsewhere):
    /// never asked under a lock.
    pub(crate) fn alive(&self) -> bool {
        restart_stamp::alive_as(self.pid, &self.start)
    }
}

/// The pid a self-claim's holder names: `codex-alab@<pid>` in its one
/// canonical spelling (no sign, no leading zero), above 1. `None` for any
/// other holder.
#[must_use]
pub(crate) fn holder_pid(holder: &str) -> Option<u32> {
    let digits = holder.strip_prefix(SELF_CLAIM_PREFIX)?;
    let pid = digits.parse::<u32>().ok().filter(|p| *p > 1)?;
    (pid.to_string() == digits).then_some(pid)
}

/// The proof's facts, as the server read them for one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Facts {
    /// The kernel's peer pid of the connection the request came on, read
    /// right after the request line; `None` off a direct local connection or
    /// where it cannot be read.
    pub(crate) peer: Option<u32>,
    /// The session's foreground process group, read live from its PTY master
    /// (`<= 0`: none, or unreadable).
    pub(crate) foreground: i32,
    /// Whether the named pid's image is an ALab build ([`runs_alab_build`]),
    /// read only once the other facts hold.
    pub(crate) alab_image: bool,
}

/// Whether `holder`'s claim is proven by `facts` (pure): it names a pid that
/// is the connection's peer, leads the foreground group and runs an ALab
/// build. The identity is read by [`prove`].
#[must_use]
pub(crate) fn proves(holder: &str, facts: Facts) -> Option<u32> {
    let pid = holder_pid(holder)?;
    let leads = i64::from(facts.foreground) == i64::from(pid);
    (facts.peer == Some(pid) && leads && facts.alab_image).then_some(pid)
}

/// Whether process `pid`'s kernel image (`proc_pidpath`) is an ALab build of
/// Codex ([`upgrade_codex::alab_bytes`]). `false` where the image cannot be
/// read. A syscall and a few file reads: never under a lock.
#[must_use]
pub(crate) fn runs_alab_build(pid: u32) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(aterm_uds::sudo::exe_path)
        .is_some_and(|exe| upgrade_codex::alab_bytes(&exe))
}

/// The proof of `holder`'s claim on `facts`, with the agent's kernel start
/// ([`Proven`]); `None` when any fact fails — or the start cannot be read, so
/// no later look could tell the process from a reused pid (fail closed).
#[must_use]
pub(crate) fn prove(holder: &str, facts: Facts) -> Option<Proven> {
    let pid = proves(holder, facts)?;
    let start = restart_stamp::kernel_start(pid)?;
    Some(Proven { pid, start })
}

// ---------------------------------------------------------------------------
// ATERM'S OWN COPY of codex-alab's session record (the contract, §4).
// ---------------------------------------------------------------------------

/// The copy each session's proven claim was last read into, by tab
/// ([`refresh`]): the record read ONLY while the claim's identity was live —
/// at the proven claim and at each `meta-change` of the session — so the
/// exit is decided and relaunched from what the agent itself left
/// ([`aterm_agent::harness::self_claimed`]), never from the file as anyone
/// rewrote it after the agent died. Kept past the claim's clear, for the
/// exit's reader; replaced by the next identity's read.
static COPIES: Mutex<Copies> = Mutex::new(Copies::new());

/// The copies by tab (a `BTreeMap`, so the static needs no lazy init).
type Copies = std::collections::BTreeMap<String, Copy>;

/// Read the proven claim's record now, while its identity is live
/// ([`self_claimed::read_copy`]: looked at before and after the read), and
/// keep it as aterm's copy. A read that cannot be vouched for keeps the copy
/// it had. Its revision is checked again before publication: a release or
/// renewal during the off-lock read invalidates it. Returns whether the claim
/// still stands. Never on the render or input path.
fn refresh(ctx: &crate::SessionCtx, proven: &Proven, revision: u64) -> bool {
    refresh_by(ctx, proven, revision, || {
        self_claimed::read_copy(proven.pid, &proven.start)
    })
}

fn refresh_by(
    ctx: &crate::SessionCtx,
    proven: &Proven,
    revision: u64,
    read: impl FnOnce() -> Option<Copy>,
) -> bool {
    let copy = read();
    if let Some(copy) = &copy
        && let Err(why) = &copy.resume
    {
        aterm_log::info!(
            "self-claim @{}: codex-alab {}'s record names no line aterm would relaunch: {why}",
            ctx.self_id,
            proven.pid
        );
    }
    let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
    if meta.supervisor_revision != revision
        || meta
            .live_claim(crate::metrics::now_us())
            .and_then(|claim| claim.proven.as_ref())
            != Some(proven)
    {
        return false;
    }
    if let Some(copy) = copy {
        COPIES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(ctx.self_id.to_string(), copy);
    }
    true
}

/// Tab `sid`'s live proven claim, read under its meta guard (a clone; the
/// guard let go before anything is read from disk).
fn standing(ctx: &crate::SessionCtx) -> Option<(Proven, u64)> {
    let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
    meta.live_claim(crate::metrics::now_us())
        .and_then(|claim| claim.proven.clone())
        .map(|proven| (proven, meta.supervisor_revision))
}

/// A `meta-change` of the session: its proven claim's record is read again
/// while the identity is live (§4, "at each meta-change").
pub(crate) fn meta_changed(ctx: &crate::SessionCtx) {
    if let Some((proven, revision)) = standing(ctx) {
        refresh(ctx, &proven, revision);
    }
}

/// The copy kept for tab `sid`, whatever identity it names (the exit's
/// reader matches it to the agent it read the exit of).
#[must_use]
pub(crate) fn copy_of_tab(sid: &str) -> Option<Copy> {
    COPIES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(sid)
        .cloned()
}

/// The copy of the process `pid` started at `start` is TAKEN by the reader
/// of that process's exit: one exit, one decision — a later exit in the tab
/// (another agent's, with no snapshot to tell them apart) never meets it.
pub(crate) fn take(sid: &str, pid: u32, start: &str) {
    let mut copies = COPIES.lock().unwrap_or_else(PoisonError::into_inner);
    if copies
        .get(sid)
        .is_some_and(|copy| copy.pid == pid && copy.start == start)
    {
        copies.remove(sid);
    }
}

/// Tab `sid`'s copy is dropped where its process has ended unread: an exit
/// nobody decided from it (its worker was switched off, or was reading
/// another program's exit) is never decided from it later.
pub(crate) fn discard_dead(sid: &str) {
    let dead = copy_of_tab(sid).filter(|copy| !restart_stamp::alive_as(copy.pid, &copy.start));
    if let Some(copy) = dead {
        take(sid, copy.pid, &copy.start);
    }
}

/// Tab `sid` closed: nothing of its agent is read again.
pub(crate) fn forget(sid: &str) {
    COPIES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(sid);
}

/// A proven claim was GRANTED on tab `sid` ([`crate::session_timeline::
/// claim_supervisor_as`], replacing `replaced` where it did): the record is
/// read while the identity is live, and — where the claim MOVED (`moved`: it
/// was taken, gained its proof, or replaced another) — the window host is
/// told at once ([`crate::harness_host::self_claimed`]). A renewal (the
/// agent's per-act re-confirm) reads the record and tells nobody.
pub(crate) fn claimed(
    ctx: &crate::SessionCtx,
    proven: &Proven,
    revision: u64,
    moved: bool,
    replaced: Option<&str>,
) {
    if refresh(ctx, proven, revision) && (moved || replaced.is_some()) {
        crate::harness_host::self_claimed(ctx.self_id.as_str(), replaced);
    }
}

/// A proven claim on tab `sid` was CLEARED (released, lapsed, or its process
/// gone from the front): its record is read once more while the identity is
/// still live (an orderly end removes it first), and the window host is
/// told whether the agent goes on without it (`true`: codex-alab went
/// passive, so aterm's own lane takes the session back) or is gone.
///
/// A claim given back by an agent that GOES ON — its record standing with no
/// word of an end ([`self_claimed::shows_end`]: passive, or an `execve` into
/// its next build) — leaves no copy: nothing is read from it again while no
/// proof stands, so its later exit is aterm's own lane's to read, as for any
/// Codex, and never decided from a copy frozen at the release (a `/new` or a
/// fork since, an orderly `/exit` it never saw). A next proven claim reads a
/// fresh copy. A release that shows the end coming (the record removed, an
/// exit word) keeps it, for the exit's reader.
pub(crate) fn released(ctx: &crate::SessionCtx, proven: &Proven, revision: u64) {
    if let Some(alive) = released_by(
        ctx,
        proven,
        revision,
        || proven.alive(),
        || self_claimed::read_copy(proven.pid, &proven.start),
    ) {
        crate::harness_host::self_claim_released(ctx.self_id.as_str(), alive);
    }
}

fn released_by(
    ctx: &crate::SessionCtx,
    proven: &Proven,
    revision: u64,
    mut alive: impl FnMut() -> bool,
    read: impl FnOnce() -> Option<Copy>,
) -> Option<bool> {
    let copy = alive().then(read).flatten();
    // Death during read_copy invalidates that read. It is a crash with the
    // previous copy retained, not a passive handback while still running.
    let alive = alive();
    finish_release(ctx, proven, revision, alive, copy).then_some(alive)
}

/// Commit the final live read only at the release that admitted it. The meta
/// guard also excludes earlier refreshes until the keep/remove decision ends.
fn finish_release(
    ctx: &crate::SessionCtx,
    proven: &Proven,
    revision: u64,
    alive: bool,
    copy: Option<Copy>,
) -> bool {
    let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
    if meta.supervisor_revision != revision || meta.supervisor.is_some() {
        return false;
    }
    let mut copies = COPIES.lock().unwrap_or_else(PoisonError::into_inner);
    let sid = ctx.self_id.as_str();
    if let Some(copy) = copy {
        copies.insert(sid.to_string(), copy);
    }
    if alive
        && copies
            .get(sid)
            .filter(|copy| copy.pid == proven.pid && copy.start == proven.start)
            .is_none_or(|copy| !self_claimed::shows_end(copy))
    {
        copies.remove(sid);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_holder_names_its_pid_in_one_spelling() {
        assert_eq!(holder_pid("codex-alab@4242"), Some(4242));
        // Negative controls: another holder, a non-canonical or absent pid.
        for other in [
            "aterm-harness@4242",
            "codex-alab@",
            "codex-alab@04242",
            "codex-alab@+4242",
            "codex-alab@1",
            "codex-alab@0",
            "codex-alab@42x",
            "codex-alab-4242",
        ] {
            assert_eq!(holder_pid(other), None, "{other}");
        }
    }

    #[test]
    fn only_the_peer_that_leads_the_foreground_proves_its_claim() {
        let facts = |peer, foreground| Facts {
            peer,
            foreground,
            alab_image: true,
        };
        assert_eq!(proves("codex-alab@500", facts(Some(500), 500)), Some(500));
        // Another program leading the group under that pid proves nothing.
        assert_eq!(
            proves(
                "codex-alab@500",
                Facts {
                    alab_image: false,
                    ..facts(Some(500), 500)
                }
            ),
            None
        );
        // A descendant (its own peer pid), a peer that does not lead, a
        // connection with no peer (the bridge, a relay), and another holder.
        assert_eq!(proves("codex-alab@500", facts(Some(501), 500)), None);
        assert_eq!(proves("codex-alab@500", facts(Some(500), 499)), None);
        assert_eq!(proves("codex-alab@500", facts(None, 500)), None);
        assert_eq!(proves("codex-alab@500", facts(Some(500), -1)), None);
        assert_eq!(proves("watcher@500", facts(Some(500), 500)), None);
    }

    #[test]
    fn record_reads_cannot_publish_after_handback_or_over_a_new_claim() {
        use crate::session_timeline::{claim_supervisor_as, release_supervisor};
        let handle = crate::session_store::test_handle_on(7511, -1);
        let ctx = handle.ctx.as_ref();
        let sid = ctx.self_id.as_str();
        let proven = Proven {
            pid: 4242,
            start: "a test identity".to_string(),
        };
        let holder = "codex-alab@4242";
        let now = crate::metrics::now_us();
        let copy = |seen| Copy {
            pid: proven.pid,
            start: proven.start.clone(),
            home: "/test/.codex".into(),
            seen,
            resume: Err("test-line"),
        };
        let standing = || {
            copy(self_claimed::Seen::Present(self_claimed::Record {
                pid: proven.pid,
                session_id: None,
                resume_argv: None,
                env: Vec::new(),
                status: Some("busy".to_string()),
                exit: None,
                cwd: None,
            }))
        };
        forget(sid);
        let first = claim_supervisor_as(ctx, holder, Some(1), None, Some(proven.clone()), now)
            .expect("a claim");
        assert!(refresh_by(
            ctx,
            &proven,
            first.revision,
            || Some(standing())
        ));
        assert!(copy_of_tab(sid).is_some(), "the standing claim publishes");

        // Handback finishes while an earlier disk read is in flight. Its
        // result must not restore the frozen pre-handback record.
        assert!(!refresh_by(ctx, &proven, first.revision, || {
            assert!(release_supervisor(ctx, None, now));
            let revision = ctx.meta.lock().unwrap().supervisor_revision;
            assert!(finish_release(
                ctx,
                &proven,
                revision,
                true,
                Some(standing())
            ));
            assert!(copy_of_tab(sid).is_none(), "passive handback keeps no copy");
            Some(standing())
        }));
        assert!(copy_of_tab(sid).is_none(), "the old read stayed invalid");

        let released_revision = ctx.meta.lock().unwrap().supervisor_revision;
        let next = claim_supervisor_as(ctx, holder, Some(2), None, Some(proven.clone()), now)
            .expect("the same identity claims again");
        assert!(refresh_by(ctx, &proven, next.revision, || Some(standing())));
        // An older release's final read cannot overwrite the next claim,
        // even when that claim names the same pid and kernel start.
        assert!(!finish_release(
            ctx,
            &proven,
            released_revision,
            true,
            Some(copy(self_claimed::Seen::Removed))
        ));
        assert!(matches!(
            copy_of_tab(sid).unwrap().seen,
            self_claimed::Seen::Present(_)
        ));

        // Positive control: the current release freezes an orderly end, for
        // the exit reader, while still refusing the earlier claim's read.
        assert!(release_supervisor(ctx, None, now));
        let revision = ctx.meta.lock().unwrap().supervisor_revision;
        assert!(finish_release(
            ctx,
            &proven,
            revision,
            true,
            Some(copy(self_claimed::Seen::Removed))
        ));
        assert!(!refresh_by(
            ctx,
            &proven,
            next.revision,
            || Some(standing())
        ));
        assert!(matches!(
            copy_of_tab(sid).unwrap().seen,
            self_claimed::Seen::Removed
        ));

        // A live identity may die while its final read is off-lock. That
        // read cannot vouch for a new copy; retain the old crash record.
        let next = claim_supervisor_as(ctx, holder, Some(3), None, Some(proven.clone()), now)
            .expect("a next claim");
        assert!(refresh_by(ctx, &proven, next.revision, || Some(standing())));
        assert!(release_supervisor(ctx, None, now));
        let revision = ctx.meta.lock().unwrap().supervisor_revision;
        let alive = std::cell::Cell::new(true);
        assert_eq!(
            released_by(
                ctx,
                &proven,
                revision,
                || alive.get(),
                || {
                    alive.set(false);
                    None
                }
            ),
            Some(false)
        );
        assert!(matches!(
            copy_of_tab(sid).unwrap().seen,
            self_claimed::Seen::Present(_)
        ));
        forget(sid);
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_proof_keeps_the_identity_of_the_process_it_named() {
        let me = std::process::id();
        let holder = format!("{SELF_CLAIM_PREFIX}{me}");
        let facts = Facts {
            peer: Some(me),
            foreground: i32::try_from(me).expect("a pid"),
            alab_image: true,
        };
        // This test binary is no ALab build: its image proves nothing.
        assert!(!runs_alab_build(me));
        let proven = prove(&holder, facts).expect("this process proves its own claim");
        assert_eq!(proven.pid, me);
        assert!(proven.alive(), "this process runs");
        // Negative control: the same pid under another start is not it.
        let reused = Proven {
            start: "Thu Jan  1 00:00:00 1970".to_string(),
            ..proven
        };
        assert!(!reused.alive(), "a reused pid is not the proof's process");
    }
}
