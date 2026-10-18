// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PROVEN SELF-CLAIM (the codex-alab native harness contract, §1–§2): an
//! agent ALab builds from source supervises ITSELF, and aterm watches beside
//! it (aterm supervises no Codex: the Codex screen lane's retirement,
//! 2026-10-16) — the claim counts only where the kernel proves it is the
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
//!   `alab-build.json` ([`codex_alab::alab_bytes`]), so another program
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
use aterm_agent::harness::{codex_alab, restart_stamp};

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
/// Codex ([`codex_alab::alab_bytes`]). `false` where the image cannot be
/// read. A syscall and a few file reads: never under a lock.
#[must_use]
pub(crate) fn runs_alab_build(pid: u32) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(aterm_uds::sudo::exe_path)
        .is_some_and(|exe| codex_alab::alab_bytes(&exe))
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

/// Whether `program`, the program in front as the status sweep publishes it
/// (`status program=`, the `argv[0]` basename), is the one a proven self-claim
/// speaks for: a Codex, by its own name or an ALab build's `codex-alab`. A wall
/// read while anything else is in front (Claude Code, a shell, a name not yet
/// resolved) is not the claimant's, so [`wall_answer`] is not asked of it.
#[must_use]
pub(crate) fn fronts_claimant(program: Option<&str>) -> bool {
    use aterm_phase::Program;
    program.is_some_and(|program| {
        // An exhaustive match, so a new agent decides here (the agent-gate guard).
        match aterm_phase::program_of(program) {
            Some(Program::Codex) => true,
            Some(
                Program::Claude | Program::Generic | Program::Agy | Program::Muse | Program::Gemini,
            ) => false,
            None => {
                let word = program.split_whitespace().next().unwrap_or(program);
                let word = word.rsplit('/').next().unwrap_or(word);
                word.eq_ignore_ascii_case("codex-alab")
            }
        }
    })
}

/// WHO ANSWERS a wall on a session under a PROVEN self-claim (the presence
/// row and tab chip): the agent itself, when it is a usage wall (the session
/// or the weekly window) and the `[harness]` policy it reads over `harness
/// policy` is in `effective` and has `resume_limits` on. codex-alab knows the
/// exact reset from its own rate-limit snapshot and waits it out, so the band
/// reads it as aterm's own harness's wall reads (`note`, "resumes by
/// itself"), never `limited`. Every other wall, or the policy off, is
/// nobody's: the stop stands for a person (fail closed).
#[must_use]
pub(crate) fn wall_answer(
    kind: aterm_phase::WallKind,
    has_reset: bool,
    policy: &aterm_agent::supervise::SupervisorConfig,
    effective: bool,
) -> aterm_messages::presence::WallAnswer {
    use aterm_agent::supervise::policy::outlook::{Answer, answer};
    use aterm_messages::presence::WallAnswer;
    use aterm_phase::WallKind;
    if !effective
        || !policy.resume_limits
        || !matches!(kind, WallKind::UsageSession | WallKind::UsageWeekly)
    {
        return WallAnswer::Nobody;
    }
    match answer(kind, has_reset, policy, true) {
        Answer::Nobody => WallAnswer::Nobody,
        Answer::Resumes => WallAnswer::Harness,
        Answer::ResumesAsks(action) => WallAnswer::HarnessAsks(action),
    }
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

/// The line the crash rule last typed in each tab ([`typed`]): what a
/// codex-alab it started, read before its record's first write, relaunches
/// on ([`self_claimed::inherited`]: only a process that runs exactly it).
static TYPED: Mutex<std::collections::BTreeMap<String, self_claimed::Resume>> =
    Mutex::new(std::collections::BTreeMap::new());

/// The crash rule types `resume` in tab `sid` (`harness_host`'s
/// `self_claimed_exit`): a copy of the process it starts that finds no
/// record yet inherits it.
pub(crate) fn typed(sid: &str, resume: &self_claimed::Resume) {
    TYPED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(sid.to_string(), resume.clone());
}

fn typed_for(sid: &str) -> Option<self_claimed::Resume> {
    TYPED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(sid)
        .cloned()
}

/// Read the proven claim's record now, while its identity is live
/// ([`self_claimed::read_copy`]: looked at before and after the read), and
/// keep it as aterm's copy. A read that cannot be vouched for keeps the copy
/// it had. Its revision is checked again before publication: a release or
/// renewal during the off-lock read invalidates it. Returns whether the claim
/// still stands. Never on the render or input path.
fn refresh(ctx: &crate::SessionCtx, proven: &Proven, revision: u64) -> bool {
    let typed = typed_for(ctx.self_id.as_str());
    refresh_by(ctx, proven, revision, || {
        self_claimed::read_copy(proven.pid, &proven.start, typed.as_ref())
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
        let mut copies = COPIES.lock().unwrap_or_else(PoisonError::into_inner);
        let copy = self_claimed::fold(copies.get(ctx.self_id.as_str()), copy);
        let line = copy.resume.clone().ok();
        let before = copies.insert(ctx.self_id.to_string(), copy);
        drop(copies);
        drop(meta);
        // The line a layout's leaf carries moved: the host follows the
        // session again, publishing it ([`crate::harness_host`]'s
        // `with_self_claimed_line`).
        if before.and_then(|copy| copy.resume.ok()) != line {
            crate::harness_host::ring();
        }
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

/// THE WATCH-ONLY WORKER'S IDLE POINT (the contract, §4): `ctx`'s live
/// proven claim names the process `pid` started at `start`, and that
/// process's own session record, read NOW while its identity is live, says it
/// rests ([`self_claimed::read_idle`]: `status: "idle"`, `draftEmpty:
/// true`). The meta guard is let go before the read. `false` with no proven
/// claim, a claim of another process, and anything the read cannot vouch for.
pub(crate) fn idle_now(ctx: &crate::SessionCtx, pid: u32, start: &str) -> bool {
    standing(ctx).is_some_and(|(proven, _)| {
        proven.pid == pid && proven.start == start && self_claimed::read_idle(pid, start)
    })
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

/// Tests: tab `sid`'s copy is `copy` (`None`: none).
#[cfg(test)]
pub(crate) fn set_copy_for_test(sid: &str, copy: Option<Copy>) {
    let mut copies = COPIES.lock().unwrap_or_else(PoisonError::into_inner);
    match copy {
        Some(copy) => copies.insert(sid.to_string(), copy),
        None => copies.remove(sid),
    };
}

/// Tab `sid` closed: nothing of its agent is read again.
pub(crate) fn forget(sid: &str) {
    COPIES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(sid);
    TYPED
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
/// passive, and aterm's worker goes on watching beside the agent itself) or
/// is gone.
///
/// A claim given back by an agent that GOES ON — its record standing with no
/// word of an end ([`self_claimed::shows_end`]: passive, or an `execve` into
/// its next build) — leaves no copy: nothing is read from it again while no
/// proof stands, so its later exit is decided by nobody and nothing relaunches
/// it (fail closed: no lane reads a Codex's end), never decided from a copy
/// frozen at the release (a `/new` or a fork since, an orderly `/exit` it
/// never saw). A next proven claim reads a fresh copy. A release that shows the end coming (the record removed, an
/// exit word) keeps it, for the exit's reader.
pub(crate) fn released(ctx: &crate::SessionCtx, proven: &Proven, revision: u64) {
    let typed = typed_for(ctx.self_id.as_str());
    if let Some(alive) = released_by(
        ctx,
        proven,
        revision,
        || proven.alive(),
        || self_claimed::read_copy(proven.pid, &proven.start, typed.as_ref()),
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
        let copy = self_claimed::fold(copies.get(sid), copy);
        copies.insert(sid.to_string(), copy);
    }
    if alive
        && copies
            .get(sid)
            .filter(|copy| copy.pid == proven.pid && copy.start == proven.start)
            .is_none_or(|copy| !self_claimed::shows_end(copy))
        && copies.remove(sid).is_some_and(|copy| copy.resume.is_ok())
    {
        drop(copies);
        drop(meta);
        // No line to carry any more: the host publishes the session again.
        crate::harness_host::ring();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a usage wall, only with the policy in effect and `resume_limits`
    /// on, is the self-claim's own to wait out; everything else stays a stop.
    #[test]
    fn a_self_claim_answers_only_a_usage_wall_under_resume_limits() {
        use aterm_agent::supervise::SupervisorConfig;
        use aterm_messages::presence::WallAnswer;
        use aterm_phase::WallKind;
        let on = SupervisorConfig::default();
        assert!(on.resume_limits, "the default resumes limits");
        let off = SupervisorConfig {
            resume_limits: false,
            ..SupervisorConfig::default()
        };
        for kind in [WallKind::UsageSession, WallKind::UsageWeekly] {
            assert_eq!(wall_answer(kind, true, &on, true), WallAnswer::Harness);
            assert_eq!(wall_answer(kind, false, &on, true), WallAnswer::Harness);
            assert_eq!(wall_answer(kind, true, &off, true), WallAnswer::Nobody);
            assert_eq!(wall_answer(kind, true, &on, false), WallAnswer::Nobody);
        }
        for kind in [
            WallKind::Spend,
            WallKind::Context,
            WallKind::Auth,
            WallKind::Memory,
            WallKind::ModelBucket { consent: false },
        ] {
            assert_eq!(
                wall_answer(kind, true, &on, true),
                WallAnswer::Nobody,
                "{kind:?}"
            );
        }
    }

    /// A self-claim speaks for a Codex in front, by either name it runs
    /// under. NEGATIVE CONTROLS: another agent, a shell, a look-alike name and
    /// a program not yet resolved are not the claimant.
    #[test]
    fn a_self_claim_speaks_only_for_a_codex_in_front() {
        for claimant in [
            "codex",
            "CODEX",
            "/opt/alab/bin/codex",
            "codex-alab",
            "Codex-Alab",
        ] {
            assert!(fronts_claimant(Some(claimant)), "{claimant}");
        }
        for other in [
            None,
            Some("claude"),
            Some("node"),
            Some("zsh"),
            Some("codexx"),
            Some("codex-alab-helper"),
            Some(""),
        ] {
            assert!(!fronts_claimant(other), "{other:?}");
        }
    }

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
                draft_empty: None,
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

        // THE KEPT COPY FOLDS ITS READS (AD-3): a read that finds nothing
        // after one that found the record keeps a REMOVAL; a first read that
        // finds nothing keeps NEVER — no orderly end it never saw.
        let next = claim_supervisor_as(ctx, holder, Some(4), None, Some(proven.clone()), now)
            .expect("a next claim");
        assert!(refresh_by(ctx, &proven, next.revision, || Some(copy(
            self_claimed::Seen::Never
        ))));
        assert_eq!(copy_of_tab(sid).unwrap().seen, self_claimed::Seen::Removed);
        forget(sid);
        assert!(refresh_by(ctx, &proven, next.revision, || Some(copy(
            self_claimed::Seen::Never
        ))));
        assert_eq!(copy_of_tab(sid).unwrap().seen, self_claimed::Seen::Never);
        forget(sid);
    }

    /// THE WATCH-ONLY WORKER'S IDLE POINT, live ([`idle_now`]): a real
    /// stand-in process (a signed copy of `sleep`, its `CODEX_HOME` the
    /// test's directory) holding the session's PROVEN claim, with a PLANTED
    /// record saying `idle` and `draftEmpty: true`, is at its idle point; the
    /// record saying `busy` is not. NEGATIVE CONTROLS: the claim's process
    /// named under another pid or start, the claim released (no proof
    /// stands), and the process gone (a dead identity) read no idle point.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn the_idle_point_is_the_proven_processs_own_record_read_live() {
        use crate::session_timeline::{claim_supervisor_as, release_supervisor};
        let dir = aterm_tempfile::TempDir::new().expect("dir");
        let home = std::fs::canonicalize(dir.path()).expect("canonical");
        let sleep = home.join("sleep");
        std::fs::copy("/bin/sleep", &sleep).expect("copy sleep");
        let signed = aterm_uds::spawnfd::command("codesign")
            .args(["--force", "--sign", "-"])
            .arg(&sleep)
            .output()
            .expect("codesign");
        assert!(signed.status.success(), "{signed:?}");
        let sleep_s = sleep.to_string_lossy().into_owned();
        let mut child = aterm_uds::spawnfd::command(&sleep)
            .arg("60")
            .env("CODEX_HOME", &home)
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        // Its exec has landed once its own argv reads back (a hang detector).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !atpkg::caller_shell::process_args(pid)
            .is_some_and(|a| a.argv.first() == Some(&sleep_s))
        {
            assert!(std::time::Instant::now() < deadline, "sleep never ran");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let start = restart_stamp::kernel_start(pid).expect("its start");
        let path = self_claimed::record_path(&home, pid);
        std::fs::create_dir_all(path.parent().expect("dir")).expect("sessions dir");
        let plant = |status: &str| {
            std::fs::write(
                &path,
                format!(
                    r#"{{"v":1,"pid":{pid},"status":"{status}","draftEmpty":true,"exit":null}}"#
                ),
            )
            .expect("the record");
        };
        let handle = crate::session_store::test_handle_on(7512, -1);
        let ctx = handle.ctx.as_ref();
        let proven = Proven {
            pid,
            start: start.clone(),
        };
        let holder = format!("{SELF_CLAIM_PREFIX}{pid}");
        let now = crate::metrics::now_us();
        claim_supervisor_as(ctx, &holder, Some(1), None, Some(proven), now).expect("a claim");
        plant("idle");
        assert!(idle_now(ctx, pid, &start), "its own record says it rests");
        plant("busy");
        assert!(!idle_now(ctx, pid, &start), "busy");
        plant("idle");
        assert!(!idle_now(ctx, pid + 1, &start), "another process");
        assert!(
            !idle_now(ctx, pid, "Thu Jan  1 00:00:00 1970"),
            "another identity"
        );
        assert!(release_supervisor(ctx, None, now));
        assert!(!idle_now(ctx, pid, &start), "no proof stands");
        claim_supervisor_as(
            ctx,
            &holder,
            Some(2),
            None,
            Some(Proven {
                pid,
                start: start.clone(),
            }),
            now,
        )
        .expect("claimed again");
        assert!(idle_now(ctx, pid, &start), "positive control, again");
        let _ = child.kill();
        let _ = child.wait();
        assert!(!idle_now(ctx, pid, &start), "a dead identity rests nowhere");
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
