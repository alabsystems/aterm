// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kernel-proven `codex-alab@<pid>` self-claim verification and live session-record copies.

use std::sync::{Mutex, PoisonError};

use aterm_agent::harness::self_claimed::{self, Copy};
use aterm_agent::harness::{codex_alab, restart_stamp};

/// The holder prefix a self-claim is made under: `codex-alab@<pid>`.
pub(crate) const SELF_CLAIM_PREFIX: &str = "codex-alab@";

/// Kernel-proven self-claim identity (`pid` and kernel `start`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Proven {
    /// Agent pid (connection peer and foreground process group leader at the claim).
    pub(crate) pid: u32,
    /// Its kernel start, as [`restart_stamp::kernel_start`] spells it.
    pub(crate) start: String,
}

impl Proven {
    /// Whether the proven process (`pid` and `start`) is still alive.
    pub(crate) fn alive(&self) -> bool {
        restart_stamp::alive_as(self.pid, &self.start)
    }
}

/// Parse canonical `<pid>` (> 1) from a `codex-alab@<pid>` holder string.
#[must_use]
pub(crate) fn holder_pid(holder: &str) -> Option<u32> {
    let digits = holder.strip_prefix(SELF_CLAIM_PREFIX)?;
    let pid = digits.parse::<u32>().ok().filter(|p| *p > 1)?;
    (pid.to_string() == digits).then_some(pid)
}

/// The proof's facts, as the server read them for one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Facts {
    /// Kernel peer pid of the requesting local connection.
    pub(crate) peer: Option<u32>,
    /// Session foreground process group read live from its PTY master.
    pub(crate) foreground: i32,
    /// Whether the named pid's executable image is an ALab build.
    pub(crate) alab_image: bool,
}

/// Return the claimed pid if `facts` prove `holder`'s self-claim.
#[must_use]
pub(crate) fn proves(holder: &str, facts: Facts) -> Option<u32> {
    let pid = holder_pid(holder)?;
    let leads = i64::from(facts.foreground) == i64::from(pid);
    (facts.peer == Some(pid) && leads && facts.alab_image).then_some(pid)
}

/// Whether process `pid`'s executable image is an ALab build of Codex.
#[must_use]
pub(crate) fn runs_alab_build(pid: u32) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(aterm_uds::sudo::exe_path)
        .is_some_and(|exe| codex_alab::alab_bytes(&exe))
}

/// Prove `holder`'s claim on `facts` and capture its kernel start.
#[must_use]
pub(crate) fn prove(holder: &str, facts: Facts) -> Option<Proven> {
    let pid = proves(holder, facts)?;
    let start = restart_stamp::kernel_start(pid)?;
    Some(Proven { pid, start })
}

/// Whether foreground `program` (`status program=`) is a Codex build that a self-claim speaks for.
#[must_use]
pub(crate) fn fronts_claimant(program: Option<&str>) -> bool {
    use aterm_phase::Program;
    program.is_some_and(|program| {
        // Exhaustive over Program.
        match aterm_phase::program_of(program) {
            Some(Program::Codex) => true,
            Some(
                Program::Claude
                | Program::Grok
                | Program::Kimi
                | Program::Generic
                | Program::Agy
                | Program::Muse,
            ) => false,
            None => {
                let word = program.split_whitespace().next().unwrap_or(program);
                let word = word.rsplit('/').next().unwrap_or(word);
                word.eq_ignore_ascii_case("codex-alab")
            }
        }
    })
}

/// Return `Some("self")` when a proven self-claim answers `wall` by itself under `policy`.
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

/// The copy each session's proven claim was last read into, by tab
/// ([`refresh`]): the record read ONLY while the claim's identity was live —
/// at the proven claim and at each `meta-change` of the session — so the
/// exit is decided and relaunched from what the agent itself left
/// ([`aterm_agent::harness::self_claimed`]), never from the file as anyone
/// rewrote it after the agent died. A release while the process runs and its
/// record shows no end keeps the copy READ ONLY ([`Use::Read`]): the rollout
/// tap follows its record ([`tap_target`]), and nothing that decides an exit,
/// relaunches or carries a line sees it ([`copy_of_tab`], [`live_copy`]).
static COPIES: Mutex<Copies> = Mutex::new(Copies::new());

/// The copies by tab (a `BTreeMap`, so the static needs no lazy init).
type Copies = std::collections::BTreeMap<String, Kept>;

/// A tab's kept copy and what it is kept for.
#[derive(Clone, Debug)]
struct Kept {
    copy: Copy,
    by: Use,
}

/// What a kept copy may be used for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Use {
    /// A live proven claim's read, or its release's while the record showed the end coming or
    /// the process gone: it decides the exit, relaunches on its line and carries it.
    Claim,
    /// A release's read while its process runs on and its record shows no end: the rollout
    /// tap's only, READ ONLY. Its resume line is withdrawn at the release, and no act path is
    /// handed it.
    Read,
}

impl Kept {
    /// The copy, when it is a claim's ([`Use::Claim`]): the one every act path reads.
    fn claims(&self) -> Option<&Copy> {
        match self.by {
            Use::Claim => Some(&self.copy),
            Use::Read => None,
        }
    }

    /// The resume line a claim copy publishes to act paths (`None` for [`Use::Read`]).
    fn line(&self) -> Option<self_claimed::Resume> {
        self.claims().and_then(|copy| copy.resume.clone().ok())
    }
}

/// The line the crash rule last typed in each tab ([`typed`]): what a
/// codex-alab it started, read before its record's first write, relaunches
/// on ([`self_claimed::inherited`]: only a process that runs exactly it).
static TYPED: Mutex<std::collections::BTreeMap<String, self_claimed::Resume>> =
    Mutex::new(std::collections::BTreeMap::new());

/// Record the `resume` command typed by the crash rule in tab `sid`.
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
/// keep it as aterm's copy.
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
        let before = copies.get(ctx.self_id.as_str()).map(|kept| &kept.copy);
        let copy = self_claimed::fold(before, copy);
        let line = copy.resume.clone().ok();
        let before = copies.insert(
            ctx.self_id.to_string(),
            Kept {
                copy,
                by: Use::Claim,
            },
        );
        drop(copies);
        drop(meta);
        if before.as_ref().and_then(Kept::line) != line {
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

/// Whether `ctx`'s live proven claim matches `(pid, start)` and its live session record reports idle.
pub(crate) fn idle_now(ctx: &crate::SessionCtx, pid: u32, start: &str) -> bool {
    standing(ctx).is_some_and(|(proven, _)| {
        proven.pid == pid && proven.start == start && self_claimed::read_idle(pid, start)
    })
}

/// Re-read `ctx`'s proven claim session record on `meta-change` while its identity is live.
pub(crate) fn meta_changed(ctx: &crate::SessionCtx) {
    if let Some((proven, revision)) = standing(ctx) {
        refresh(ctx, &proven, revision);
    }
}

/// Return the kept session-record copy for tab `sid`: a claim's ([`Use::Claim`]), never the
/// read-only copy a passive release keeps.
#[must_use]
pub(crate) fn copy_of_tab(sid: &str) -> Option<Copy> {
    COPIES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(sid)
        .and_then(Kept::claims)
        .cloned()
}

/// The kept primary copy only while that exact proven claim is currently standing.
/// Post-release crash copies remain available through [`copy_of_tab`] separately.
pub(crate) fn live_copy(ctx: &crate::SessionCtx) -> Option<Copy> {
    let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
    let proven = meta.live_claim(crate::metrics::now_us())?.proven.as_ref()?;
    let copies = COPIES.lock().unwrap_or_else(PoisonError::into_inner);
    let copy = copies.get(ctx.self_id.as_str())?.claims()?;
    (copy.pid == proven.pid && copy.start == proven.start).then(|| copy.clone())
}

/// Rollout tap target for a proven `codex-alab` session: what an act path (a landing's report,
/// `result`) reads, and only ever from a live proven claim's own copy ([`rollout_target`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RolloutTarget {
    /// The live claim revision that admitted this copy.
    pub(crate) revision: u64,
    /// The agent's pid and kernel start, as the claim proved them.
    pub(crate) pid: u32,
    pub(crate) start: String,
    /// The thread whose rollout is tapped.
    pub(crate) thread: String,
    /// The `$CODEX_HOME` the record was read under (canonical).
    pub(crate) home: std::path::PathBuf,
}

/// Resolve the [`RolloutTarget`] when a proven claim and matching live record copy stand.
#[must_use]
pub(crate) fn target_of(
    proven: Option<&Proven>,
    revision: u64,
    copy: Option<&Copy>,
) -> Option<RolloutTarget> {
    let proven = proven?;
    let copy = copy.filter(|copy| copy.pid == proven.pid && copy.start == proven.start)?;
    let self_claimed::Seen::Present(record) = &copy.seen else {
        return None;
    };
    let thread = record
        .session_id
        .as_deref()
        .filter(|t| aterm_agent::harness::codex_rollout::is_thread_id(t))?;
    Some(RolloutTarget {
        revision,
        pid: proven.pid,
        start: proven.start.clone(),
        thread: thread.to_string(),
        home: copy.home.clone(),
    })
}

/// Read the [`RolloutTarget`] for `ctx` under `meta -> copies`.
#[must_use]
pub(crate) fn rollout_target(ctx: &crate::SessionCtx) -> Option<RolloutTarget> {
    let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
    let copies = COPIES.lock().unwrap_or_else(PoisonError::into_inner);
    target_of(
        meta.live_claim(crate::metrics::now_us())
            .and_then(|claim| claim.proven.as_ref()),
        meta.supervisor_revision,
        copies.get(ctx.self_id.as_str()).and_then(Kept::claims),
    )
}

/// What the rollout tap is asked to read for a tab ([`tap_target`]). Only the tap takes one:
/// nothing turns it into a [`RolloutTarget`], so no act path reads a rollout off it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TapTarget {
    /// A live proven claim's own copy names the thread, exactly as [`rollout_target`] gives it.
    Claimed(RolloutTarget),
    /// READ ONLY: no proven claim stands, and the copy a release left ([`Use::Read`]) is of a
    /// process alive and leading the tab's front. The tap reads that process's record again at
    /// each ask and taps the thread it names then ([`tapped`]).
    Watched(Watched),
}

/// A released claim's process, which the rollout tap follows by its own record while no claim
/// stands ([`TapTarget::Watched`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Watched {
    /// The tab's claim revision when this target was named: any claim or release since names
    /// another, so a target named before it no longer holds.
    pub(crate) revision: u64,
    /// The process's pid and kernel start, as the released claim proved them.
    pub(crate) pid: u32,
    pub(crate) start: String,
    /// The `$CODEX_HOME` its record was read under (canonical).
    pub(crate) home: std::path::PathBuf,
}

impl From<RolloutTarget> for TapTarget {
    fn from(target: RolloutTarget) -> Self {
        Self::Claimed(target)
    }
}

impl TapTarget {
    /// The process whose rollout is read.
    pub(crate) const fn pid(&self) -> u32 {
        match self {
            Self::Claimed(target) => target.pid,
            Self::Watched(watched) => watched.pid,
        }
    }

    /// That process's kernel start, as its claim proved it.
    pub(crate) fn start(&self) -> &str {
        match self {
            Self::Claimed(target) => &target.start,
            Self::Watched(watched) => &watched.start,
        }
    }

    /// The `$CODEX_HOME` its record was read under (canonical).
    pub(crate) fn home(&self) -> &std::path::Path {
        match self {
            Self::Claimed(target) => &target.home,
            Self::Watched(watched) => &watched.home,
        }
    }
}

/// The rollout one ask of the tap reads: its [`TapTarget`] and the thread that target names at
/// the ask ([`tapped`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tapped {
    pub(crate) target: TapTarget,
    pub(crate) thread: String,
}

/// Why an ask taps no rollout ([`tapped`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Untapped {
    /// The tab's claim or copy no longer gives the asked target, or its process was not live
    /// across the record's read.
    Moved,
    /// The read-only copy's record, read again, names no thread of its own process.
    Unnamed,
}

/// The [`TapTarget`] a tab's standing gives under its `meta` guard and `kept` copy, before the
/// front is looked at: a live proven claim's own copy's ([`target_of`]); with none, the read-only
/// copy's process while its record shows no end coming ([`self_claimed::shows_end`]).
fn standing_of(
    meta: &crate::session_timeline::SessionMeta,
    kept: Option<&Kept>,
) -> Option<TapTarget> {
    let revision = meta.supervisor_revision;
    if let Some(proven) = meta
        .live_claim(crate::metrics::now_us())
        .and_then(|claim| claim.proven.as_ref())
    {
        return target_of(Some(proven), revision, kept.and_then(Kept::claims))
            .map(TapTarget::Claimed);
    }
    let copy = &kept.filter(|kept| kept.by == Use::Read)?.copy;
    (!self_claimed::shows_end(copy)).then(|| {
        TapTarget::Watched(Watched {
            revision,
            pid: copy.pid,
            start: copy.start.clone(),
            home: copy.home.clone(),
        })
    })
}

/// The thread `target` names with `kept` the tab's copy: a claim's, its own; a read-only copy's,
/// the `sessionId` its record names, when that record is its own process's and the id a
/// thread's.
fn thread_of(target: &TapTarget, kept: Option<&Kept>) -> Option<String> {
    match target {
        TapTarget::Claimed(target) => Some(target.thread.clone()),
        TapTarget::Watched(watched) => {
            let self_claimed::Seen::Present(record) = &kept?.copy.seen else {
                return None;
            };
            if record.pid != watched.pid {
                return None;
            }
            record
                .session_id
                .as_deref()
                .filter(|t| aterm_agent::harness::codex_rollout::is_thread_id(t))
                .map(str::to_owned)
        }
    }
}

/// THE ROLLOUT TAP'S TARGET for tab `ctx`, whose foreground group the sweep read as
/// `foreground`: a live proven claim's own copy's, exactly [`rollout_target`]'s; with no proven
/// claim, READ ONLY, the copy a release left while its process runs on ([`Use::Read`]), and only
/// while that process leads `foreground` and is alive as the start its claim proved
/// ([`restart_stamp::alive_as`]).
#[must_use]
pub(crate) fn tap_target(ctx: &crate::SessionCtx, foreground: i32) -> Option<TapTarget> {
    tap_target_by(ctx, foreground, restart_stamp::alive_as)
}

fn tap_target_by(
    ctx: &crate::SessionCtx,
    foreground: i32,
    alive: impl FnOnce(u32, &str) -> bool,
) -> Option<TapTarget> {
    let target = {
        let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
        let copies = COPIES.lock().unwrap_or_else(PoisonError::into_inner);
        standing_of(&meta, copies.get(ctx.self_id.as_str()))?
    };
    match &target {
        TapTarget::Claimed(_) => Some(target),
        TapTarget::Watched(watched) => (i64::from(foreground) == i64::from(watched.pid)
            && alive(watched.pid, &watched.start))
        .then_some(target),
    }
}

/// THE ASK'S ROLLOUT, on the tap's thread at each ask. A claim's target taps its own thread. A
/// read-only one first reads its process's record again by `record` (off every lock:
/// [`self_claimed::read_seen`] live), since codex-alab writes that record whether or not it
/// holds a claim and a `/new` or `/resume` moves its `sessionId`. Under `meta -> copies`, while
/// the standing still gives `target`, the copy takes that read (folded, so a record gone after one
/// was seen reads removed), and the thread is the one the copy names then: a moved thread is
/// tapped at once and the old one never again. [`Untapped::Moved`] when the standing no longer
/// gives `target`, before the read or after it, or the identity was not live across the read;
/// [`Untapped::Unnamed`] when the record names no thread now, which taps nothing until it names
/// one.
pub(crate) fn tapped(
    ctx: &crate::SessionCtx,
    target: &TapTarget,
    record: fn(&std::path::Path, u32, &str) -> Option<self_claimed::Seen>,
) -> Result<Tapped, Untapped> {
    let seen = match target {
        TapTarget::Claimed(_) => None,
        TapTarget::Watched(watched) => {
            Some(record(&watched.home, watched.pid, &watched.start).ok_or(Untapped::Moved)?)
        }
    };
    let sid = ctx.self_id.as_str();
    let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
    let mut copies = COPIES.lock().unwrap_or_else(PoisonError::into_inner);
    if standing_of(&meta, copies.get(sid)).as_ref() != Some(target) {
        return Err(Untapped::Moved);
    }
    if let Some(seen) = seen
        && let Some(kept) = copies.get_mut(sid)
    {
        let read = Copy {
            seen,
            ..kept.copy.clone()
        };
        kept.copy = self_claimed::fold(Some(&kept.copy), read);
    }
    let kept = copies.get(sid);
    if standing_of(&meta, kept).as_ref() != Some(target) {
        return Err(Untapped::Moved);
    }
    let thread = thread_of(target, kept).ok_or(Untapped::Unnamed)?;
    Ok(Tapped {
        target: target.clone(),
        thread,
    })
}

/// Guard holding `meta` and `copies` locks across a bounded timeline hold.
pub(crate) struct RolloutGuard<'a> {
    _meta: std::sync::MutexGuard<'a, crate::session_timeline::SessionMeta>,
    _copies: std::sync::MutexGuard<'static, Copies>,
}

/// Hold `meta -> copies` while the tab's standing still gives `target`, whatever thread it names
/// (the tap's capture of the run it reads).
pub(crate) fn hold_target<'a>(
    ctx: &'a crate::SessionCtx,
    target: &TapTarget,
) -> Option<RolloutGuard<'a>> {
    let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
    let copies = COPIES.lock().unwrap_or_else(PoisonError::into_inner);
    let holds = standing_of(&meta, copies.get(ctx.self_id.as_str())).as_ref() == Some(target);
    holds.then_some(RolloutGuard {
        _meta: meta,
        _copies: copies,
    })
}

/// Hold `meta -> copies` while the tab's standing still gives `tapped`'s target and that target
/// still names `tapped`'s thread.
pub(crate) fn hold_rollout<'a>(
    ctx: &'a crate::SessionCtx,
    tapped: &Tapped,
) -> Option<RolloutGuard<'a>> {
    let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
    let copies = COPIES.lock().unwrap_or_else(PoisonError::into_inner);
    let holds = {
        let kept = copies.get(ctx.self_id.as_str());
        standing_of(&meta, kept).as_ref() == Some(&tapped.target)
            && thread_of(&tapped.target, kept).as_deref() == Some(tapped.thread.as_str())
    };
    holds.then_some(RolloutGuard {
        _meta: meta,
        _copies: copies,
    })
}

/// Take and remove the kept record copy for `(pid, start)` in tab `sid`.
pub(crate) fn take_copy(sid: &str, pid: u32, start: &str) {
    let mut copies = COPIES.lock().unwrap_or_else(PoisonError::into_inner);
    if copies
        .get(sid)
        .is_some_and(|kept| kept.copy.pid == pid && kept.copy.start == start)
    {
        copies.remove(sid);
    }
}

/// Drop tab `sid`'s kept record copy, a claim's or a read-only one, if its process has ended.
pub(crate) fn discard_dead(sid: &str) {
    let dead = COPIES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(sid)
        .map(|kept| (kept.copy.pid, kept.copy.start.clone()))
        .filter(|(pid, start)| !restart_stamp::alive_as(*pid, start));
    if let Some((pid, start)) = dead {
        take_copy(sid, pid, &start);
    }
}

/// Tests: tab `sid`'s copy is `copy`, a claim's (`None`: none).
#[cfg(test)]
pub(crate) fn set_copy_for_test(sid: &str, copy: Option<Copy>) {
    let mut copies = COPIES.lock().unwrap_or_else(PoisonError::into_inner);
    match copy {
        Some(copy) => copies.insert(
            sid.to_string(),
            Kept {
                copy,
                by: Use::Claim,
            },
        ),
        None => copies.remove(sid),
    };
}

/// Tests: [`finish_release`] of `proven`'s claim at `revision` on `ctx`, its process `alive` or
/// not and `copy` the release's own read, as [`released`] runs it on the kernel's word.
#[cfg(test)]
pub(crate) fn released_for_test(
    ctx: &crate::SessionCtx,
    proven: &Proven,
    revision: u64,
    alive: bool,
    copy: Option<Copy>,
) -> bool {
    finish_release(ctx, proven, revision, alive, copy)
}

/// Tab `sid` closed, whether or not a harness host runs to [`forget`] it: its read-only copy
/// ([`Use::Read`]), the rollout tap's alone, goes with the tab. A claim's copy stays for the
/// host, which forgets it on its own pass once the tab is closed and no worker holds it.
pub(crate) fn forget_read(sid: &str) {
    let mut copies = COPIES.lock().unwrap_or_else(PoisonError::into_inner);
    if copies.get(sid).is_some_and(|kept| kept.by == Use::Read) {
        copies.remove(sid);
    }
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

/// Refresh the record copy when a proven claim is granted on `ctx` and notify the host if it moved.
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

/// BOOK A PROVEN SELF-CLAIM'S ACT IN ITS SESSION'S LEDGER (`<state>/drive/<sid>.jsonl`): `act`,
/// recorded by `holder` on session `sid`'s proven claim, as one
/// [`aterm_agent::supervise::approvals::SelfClaimRow`] through the ledger's one append, the one
/// aterm's own loop writes its rows through, under the lock the loop's cut of the ledger holds
/// ([`aterm_agent::supervise::approvals::book_self_claim`]) — the row Settings ▸ Harness counts
/// as the holder's and `aterm harness ledger @<sid>` lists. Blocking file I/O on the control lane
/// that admitted the act, with no session lock held; whether the row was written (a failure is
/// logged).
pub(crate) fn book_act(sid: &str, holder: &str, act: &str) -> bool {
    use aterm_agent::supervise::approvals;
    let Some(path) =
        aterm_types::dirs::state_dir().and_then(|root| approvals::path_under(&root, Some(sid)))
    else {
        aterm_log::info!("self-claim @{sid}: act {act}: no state directory to book it in");
        return false;
    };
    let mut refused = Vec::new();
    let written = approvals::book_self_claim(
        &path,
        Some(sid),
        aterm_agent::supervise::journal::unix_ms(),
        &approvals::SelfClaimRow { holder, act },
        &mut refused,
    )
    .is_some();
    if !written {
        aterm_log::info!(
            "self-claim @{sid}: act {act}: not booked: {}",
            String::from_utf8_lossy(&refused).trim()
        );
    }
    written
}

/// Refresh the record copy when a proven claim is released on `ctx`, keep it for the exit or read
/// only ([`keep_to_read`]), and notify the host.
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
    let before = copies.get(sid).and_then(Kept::line);
    if let Some(copy) = copy {
        let copy = self_claimed::fold(copies.get(sid).map(|kept| &kept.copy), copy);
        copies.insert(
            sid.to_string(),
            Kept {
                copy,
                by: Use::Claim,
            },
        );
    }
    if alive {
        keep_to_read(&mut copies, sid, proven);
    }
    let after = copies.get(sid).and_then(Kept::line);
    drop(copies);
    drop(meta);
    if before != after {
        crate::harness_host::ring();
    }
    true
}

/// THE PASSIVE RELEASE KEEPS ITS COPY TO READ: what a release of `proven`'s claim leaves in tab
/// `sid` while that process still runs. A copy of another process goes; one whose record shows
/// the end coming stays the claim's, for the exit; any other is kept READ ONLY
/// ([`Use::Read`]), its resume line withdrawn, so nothing decides the exit, relaunches or carries
/// a line from it, and the rollout tap reads on ([`tap_target`]).
fn keep_to_read(copies: &mut Copies, sid: &str, proven: &Proven) {
    let Some(kept) = copies.get_mut(sid) else {
        return;
    };
    if kept.copy.pid != proven.pid || kept.copy.start != proven.start {
        copies.remove(sid);
        return;
    }
    if self_claimed::shows_end(&kept.copy) {
        return;
    }
    kept.by = Use::Read;
    kept.copy.resume = Err("released");
}

/// Stand-in child process helper for self-claim tests.
#[cfg(test)]
pub(crate) mod stand_in {
    use super::Proven;

    /// What tells [`a_stand_in_agent_when_asked`] to stand in.
    const STAND_IN_ENV: &str = "ATERM_SELF_CLAIM_TEST_STAND_IN";

    /// THE STAND-IN'S BODY: this test binary, which waits for its parent's
    /// end of the pipe to close.
    #[test]
    fn a_stand_in_agent_when_asked() {
        use std::io::Read as _;
        if std::env::var_os(STAND_IN_ENV).is_none() {
            return;
        }
        let _ = std::io::stdin().read(&mut [0u8; 1]);
    }

    /// Pid of the shared stand-in child process started under a scratch `HOME`.
    pub(crate) fn pid() -> u32 {
        use aterm_uds::spawnfd::{Child, Stdio, command};
        static STAND_IN: std::sync::OnceLock<Child> = std::sync::OnceLock::new();
        STAND_IN
            .get_or_init(|| {
                let root = aterm_types::dirs::test_root().expect("this test binary's root");
                let home = root.join("stand-in");
                std::fs::create_dir_all(&home).expect("the stand-in's home");
                command(std::env::current_exe().expect("the test binary"))
                    .args([
                        "self_claim::stand_in::a_stand_in_agent_when_asked",
                        "--exact",
                    ])
                    .env_clear()
                    .env(STAND_IN_ENV, "1")
                    .env("HOME", &home)
                    .env("CODEX_HOME", home.join(".codex"))
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("the stand-in")
            })
            .id()
    }

    /// [`Proven`] identity of the running stand-in child process.
    pub(crate) fn running() -> Proven {
        let pid = pid();
        Proven {
            pid,
            start: aterm_agent::harness::restart_stamp::kernel_start(pid).expect("its start"),
        }
    }

    /// A codex-alab stand-in of a test's own: the kept signed `sleep`, run in place with
    /// `CODEX_HOME` the test's directory, whose session record the test plants
    /// ([`Self::plant`]). Killed and reaped when dropped.
    #[cfg(target_vendor = "apple")]
    pub(crate) struct Sleeper {
        child: aterm_uds::spawnfd::Child,
        home: std::path::PathBuf,
        /// Its pid and kernel start.
        pub(crate) proven: Proven,
    }

    #[cfg(target_vendor = "apple")]
    impl Sleeper {
        /// Starts one under `home` (canonical) and waits until it runs from the stand-in.
        pub(crate) fn under(home: &std::path::Path) -> Self {
            let sleep = std::fs::canonicalize(aterm_uds::deadline::stand_in(std::path::Path::new(
                "/bin/sleep",
            )))
            .expect("the kept stand-in");
            let sleep_s = sleep.to_string_lossy().into_owned();
            let child = aterm_uds::spawnfd::command(&sleep)
                .arg("60")
                .env("CODEX_HOME", home)
                .spawn()
                .expect("spawn sleep");
            let pid = child.id();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while !atpkg::caller_shell::process_args(pid)
                .is_some_and(|a| a.argv.first() == Some(&sleep_s))
            {
                assert!(std::time::Instant::now() < deadline, "sleep never ran");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let start = aterm_agent::harness::restart_stamp::kernel_start(pid).expect("its start");
            Self {
                child,
                home: home.to_path_buf(),
                proven: Proven { pid, start },
            }
        }

        /// Writes its session record naming `session` (`None`: `sessionId` null), busy and with
        /// no exit word, as codex-alab's writer does with or without a claim.
        pub(crate) fn plant(&self, session: Option<&str>) {
            self.plant_as(self.proven.pid, session, "busy");
        }

        /// Writes, at its own record's path, a record that says it is `pid`'s, names `session`
        /// and says `status`, with no exit word.
        pub(crate) fn plant_as(&self, pid: u32, session: Option<&str>, status: &str) {
            let path = self.record();
            std::fs::create_dir_all(path.parent().expect("dir")).expect("sessions dir");
            let session = session.map_or("null".to_string(), |s| format!("\"{s}\""));
            std::fs::write(
                &path,
                format!(
                    r#"{{"v":1,"pid":{pid},"sessionId":{session},"status":"{status}","exit":null}}"#
                ),
            )
            .expect("the record");
        }

        /// Removes its session record, as codex-alab does at its end.
        pub(crate) fn unplant(&self) {
            std::fs::remove_file(self.record()).expect("the record removed");
        }

        fn record(&self) -> std::path::PathBuf {
            aterm_agent::harness::self_claimed::record_path(&self.home, self.proven.pid)
        }

        /// Kills it and waits for its end.
        pub(crate) fn end(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[cfg(target_vendor = "apple")]
    impl Drop for Sleeper {
        fn drop(&mut self) {
            self.end();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_snapshot_copy_requires_its_current_proven_claim() {
        use crate::session_timeline::{claim_supervisor_as, release_supervisor};
        let handle = crate::session_store::test_handle_on(7593, -1);
        let ctx = handle.ctx.as_ref();
        let sid = ctx.self_id.as_str();
        let now = crate::metrics::now_us();
        let proven = Proven {
            pid: 4242,
            start: "current primary".to_string(),
        };
        let own = copy_of(
            proven.pid,
            &proven.start,
            present(Some("0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b77")),
        );
        set_copy_for_test(sid, Some(own.clone()));
        assert!(
            live_copy(ctx).is_none(),
            "a retained copy alone is no current claim"
        );
        claim_supervisor_as(
            ctx,
            "codex-alab@4242",
            Some(1),
            None,
            Some(proven.clone()),
            now,
            aterm_slog::Actor::Ctl,
        )
        .expect("the proven claim stands");
        assert_eq!(live_copy(ctx), Some(own.clone()));
        for other in [
            copy_of(4243, &proven.start, own.seen.clone()),
            copy_of(4242, "old process", own.seen.clone()),
        ] {
            set_copy_for_test(sid, Some(other));
            assert!(
                live_copy(ctx).is_none(),
                "a current proof cannot borrow another identity's copy"
            );
        }
        set_copy_for_test(sid, Some(own));
        assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
        assert!(
            copy_of_tab(sid).is_some(),
            "a post-release crash copy may remain"
        );
        assert!(
            live_copy(ctx).is_none(),
            "the released claim cannot disambiguate a live root"
        );
        forget(sid);
    }

    /// A copy of `pid`'s record, read under `start`, naming `session`.
    fn copy_of(pid: u32, start: &str, seen: self_claimed::Seen) -> Copy {
        Copy {
            pid,
            start: start.to_string(),
            home: std::path::PathBuf::from("/private/tmp/x/codex-home"),
            seen,
            resume: Err("no-resume-line"),
        }
    }

    fn present(session: Option<&str>) -> self_claimed::Seen {
        self_claimed::Seen::Present(self_claimed::Record {
            pid: 4242,
            session_id: session.map(str::to_string),
            resume_argv: None,
            env: Vec::new(),
            status: Some("busy".to_string()),
            exit: None,
            cwd: None,
            draft_empty: None,
            frame: None,
            goal: None,
            initializing: false,
        })
    }

    /// THE ROLLOUT TAP'S CLAIM GATE: only a
    /// proven claim's own copy, standing and naming a thread id, names a
    /// rollout to tap — its thread, under the home the record was read
    /// under.
    #[test]
    fn only_a_proven_claims_own_copy_names_a_rollout_target() {
        const THREAD: &str = "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b77";
        let start = "Mon Oct 20 16:37:58 2026";
        let proven = Proven {
            pid: 4242,
            start: start.to_string(),
        };
        let own = copy_of(4242, start, present(Some(THREAD)));
        assert_eq!(
            target_of(Some(&proven), 7, Some(&own)),
            Some(RolloutTarget {
                revision: 7,
                pid: 4242,
                start: start.to_string(),
                thread: THREAD.to_string(),
                home: std::path::PathBuf::from("/private/tmp/x/codex-home"),
            })
        );
        assert_eq!(
            target_of(None, 7, Some(&own)),
            None,
            "a vendor Codex: no proof"
        );
        assert_eq!(target_of(Some(&proven), 7, None), None, "no copy read yet");
        for other in [
            copy_of(4243, start, present(Some(THREAD))),
            copy_of(4242, "Mon Oct 20 09:00:00 2026", present(Some(THREAD))),
            copy_of(4242, start, self_claimed::Seen::Removed),
            copy_of(4242, start, self_claimed::Seen::Never),
            copy_of(4242, start, present(None)),
            copy_of(4242, start, present(Some("not-a-thread"))),
        ] {
            assert_eq!(target_of(Some(&proven), 7, Some(&other)), None, "{other:?}");
        }
    }

    /// THE PASSIVE RELEASE KEEPS ITS COPY TO READ ([`keep_to_read`]): a proven claim released
    /// while its process runs and its record shows no end keeps the copy READ ONLY. No act path
    /// sees it ([`copy_of_tab`], [`live_copy`], [`rollout_target`]) and its resume line is gone
    /// even from the kept entry; the rollout tap's target stands on it ([`tap_target`]: the same
    /// process and home, at the release's revision) while that process leads the front and is
    /// alive as its claim proved it, and the sweep keeps asking (`Ask::Read`, never `Stop`). A
    /// claim granted again names exactly the claim's target again, at its own revision.
    /// NEGATIVE CONTROLS: another process leading the front, a process not alive as that start
    /// (dead, or another start under the pid), the real kernel's word on an identity no process
    /// has, and a release whose record shows the end coming (the claim's copy kept for the exit)
    /// name no read-only target; a copy of another process goes at the release.
    #[test]
    fn a_passive_release_keeps_its_copy_to_read_and_no_act_path_sees_it() {
        use crate::codex_rollout_tap::{Ask, RECHECK, due};
        use crate::session_timeline::{claim_supervisor_as, release_supervisor};
        const THREAD: &str = "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b77";
        let handle = crate::session_store::test_handle_on(7594, -1);
        let ctx = handle.ctx.as_ref();
        let sid = ctx.self_id.as_str();
        let proven = Proven {
            pid: 4242,
            start: "Mon Oct 20 16:37:58 2026".to_string(),
        };
        let now = crate::metrics::now_us();
        let claim = |conn| {
            claim_supervisor_as(
                ctx,
                "codex-alab@4242",
                Some(conn),
                None,
                Some(proven.clone()),
                now,
                aterm_slog::Actor::Ctl,
            )
            .expect("the proven claim stands")
            .revision
        };
        let lined = || Copy {
            resume: Ok(self_claimed::Resume {
                argv: vec!["/opt/alab/bin/codex".into(), "resume".into(), THREAD.into()],
                session: THREAD.to_string(),
                codex_home: None,
                codex_auth_path: None,
                cwd: None,
            }),
            ..copy_of(4242, &proven.start, present(Some(THREAD)))
        };
        let revision = ctx.meta.lock().unwrap().supervisor_revision;
        let release = |copy: Copy, rings_delta: u64| {
            assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
            let revision = ctx.meta.lock().unwrap().supervisor_revision;
            let rings = crate::harness_host::rings_here();
            assert!(finish_release(ctx, &proven, revision, true, Some(copy)));
            assert_eq!(crate::harness_host::rings_here(), rings + rings_delta);
            revision
        };
        let alive_as_proven = |pid: u32, start: &str| pid == 4242 && start == proven.start;
        forget(sid);
        let first = claim(1);
        assert!(first > revision);
        let rings = crate::harness_host::rings_here();
        assert!(refresh_by(ctx, &proven, first, || Some(lined())));
        assert_eq!(crate::harness_host::rings_here(), rings + 1);
        let claimed = rollout_target(ctx).expect("the claim's own target");
        assert_eq!(
            tap_target_by(ctx, 4242, alive_as_proven),
            Some(TapTarget::Claimed(claimed.clone())),
            "under the claim the tap's target is the claim's, exactly"
        );

        let released = release(lined(), 1);
        assert_eq!(copy_of_tab(sid), None, "no act path reads a passive copy");
        assert_eq!(live_copy(ctx), None);
        assert_eq!(
            rollout_target(ctx),
            None,
            "a landing or `result` finds none"
        );
        assert_eq!(
            COPIES
                .lock()
                .unwrap()
                .get(sid)
                .map(|kept| kept.copy.resume.clone()),
            Some(Err("released")),
            "the kept entry itself carries no line"
        );
        let watched = TapTarget::Watched(Watched {
            revision: released,
            pid: 4242,
            start: proven.start.clone(),
            home: std::path::PathBuf::from("/private/tmp/x/codex-home"),
        });
        assert_eq!(
            tap_target_by(ctx, 4242, alive_as_proven),
            Some(watched.clone())
        );
        let mut asked = std::collections::HashMap::new();
        let t0 = std::time::Instant::now();
        for at in [t0, t0 + RECHECK] {
            assert_eq!(
                due(
                    &mut asked,
                    7594,
                    tap_target_by(ctx, 4242, alive_as_proven),
                    at
                ),
                Ask::Read(watched.clone()),
                "the sweep keeps asking, never stops"
            );
        }
        assert_eq!(
            tap_target_by(ctx, 4243, alive_as_proven),
            None,
            "another process leads the front"
        );
        assert_eq!(
            tap_target_by(ctx, 4242, |_, _| false),
            None,
            "the process is not alive as its claim proved it"
        );
        assert_eq!(
            tap_target(ctx, 4242),
            None,
            "the kernel knows no process 4242 started then"
        );
        assert_eq!(
            due(&mut asked, 7594, tap_target(ctx, 4242), t0 + RECHECK * 2),
            Ask::Stop
        );

        let again = claim(2);
        assert!(again > released, "the claim moves the revision");
        assert_eq!(
            tap_target_by(ctx, 4242, alive_as_proven),
            None,
            "a claim no copy was read for names nothing yet, as before the release kept one"
        );
        let rings = crate::harness_host::rings_here();
        assert!(refresh_by(ctx, &proven, again, || Some(lined())));
        assert_eq!(crate::harness_host::rings_here(), rings + 1);
        assert_eq!(
            tap_target_by(ctx, 4242, alive_as_proven),
            Some(TapTarget::Claimed(RolloutTarget {
                revision: again,
                ..claimed
            }))
        );
        assert_eq!(
            copy_of_tab(sid),
            Some(lined()),
            "the claim's copy acts again"
        );

        let ending = Copy {
            seen: self_claimed::Seen::Present(self_claimed::Record {
                status: Some("exited".to_string()),
                ..match present(Some(THREAD)) {
                    self_claimed::Seen::Present(record) => record,
                    _ => unreachable!("present"),
                }
            }),
            ..lined()
        };
        let _ = release(ending.clone(), 0);
        assert_eq!(copy_of_tab(sid), Some(ending), "kept for the exit");
        assert_eq!(tap_target_by(ctx, 4242, |_, _| true), None);

        let third = claim(3);
        assert!(refresh_by(ctx, &proven, third, || Some(lined())));
        let _ = release(copy_of(4242, &proven.start, self_claimed::Seen::Never), 1);
        assert_eq!(
            copy_of_tab(sid).map(|copy| (copy.seen, copy.resume)),
            Some((self_claimed::Seen::Removed, Err("no-record"))),
            "an orderly exit that removed its record withdraws its resume line and rings the host"
        );

        let _ = claim(4);
        set_copy_for_test(
            sid,
            Some(copy_of(4243, &proven.start, present(Some(THREAD)))),
        );
        let _ = release(copy_of(4243, &proven.start, present(Some(THREAD))), 0);
        assert!(
            COPIES.lock().unwrap().get(sid).is_none(),
            "another process's copy goes"
        );
        forget(sid);
    }

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
    /// under.
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
                frame: None,
                goal: None,
                initializing: false,
            }))
        };
        forget(sid);
        let first = claim_supervisor_as(
            ctx,
            holder,
            Some(1),
            None,
            Some(proven.clone()),
            now,
            aterm_slog::Actor::Ctl,
        )
        .expect("a claim");
        assert!(refresh_by(
            ctx,
            &proven,
            first.revision,
            || Some(standing())
        ));
        assert!(copy_of_tab(sid).is_some(), "the standing claim publishes");

        assert!(!refresh_by(ctx, &proven, first.revision, || {
            assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
            let revision = ctx.meta.lock().unwrap().supervisor_revision;
            assert!(finish_release(
                ctx,
                &proven,
                revision,
                true,
                Some(standing())
            ));
            assert!(
                copy_of_tab(sid).is_none(),
                "passive handback keeps no copy an act path reads"
            );
            Some(standing())
        }));
        assert!(copy_of_tab(sid).is_none(), "the old read stayed invalid");

        let released_revision = ctx.meta.lock().unwrap().supervisor_revision;
        let next = claim_supervisor_as(
            ctx,
            holder,
            Some(2),
            None,
            Some(proven.clone()),
            now,
            aterm_slog::Actor::Ctl,
        )
        .expect("the same identity claims again");
        assert!(refresh_by(ctx, &proven, next.revision, || Some(copy(
            self_claimed::Seen::Never
        ))));
        assert_eq!(
            copy_of_tab(sid).unwrap().seen,
            self_claimed::Seen::Removed,
            "a passive read-only copy preserves that this identity's record was already seen"
        );
        assert!(refresh_by(ctx, &proven, next.revision, || Some(standing())));
        assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
        let passive_rev = ctx.meta.lock().unwrap().supervisor_revision;
        assert!(finish_release(
            ctx,
            &proven,
            passive_rev,
            true,
            Some(standing())
        ));
        let next = claim_supervisor_as(
            ctx,
            holder,
            Some(2),
            None,
            Some(proven.clone()),
            now,
            aterm_slog::Actor::Ctl,
        )
        .expect("the same identity claims again after passive release");
        let initializing = || {
            let mut c = standing();
            if let self_claimed::Seen::Present(ref mut record) = c.seen {
                record.initializing = true;
            }
            c
        };
        assert!(refresh_by(ctx, &proven, next.revision, || Some(
            initializing()
        )));
        assert_eq!(
            copy_of_tab(sid).map(|c| (c.seen, c.resume)),
            Some((
                self_claimed::Seen::Unreadable("record-regressed"),
                Err("record-regressed")
            )),
            "a passive read-only copy prevents a re-claim from regressing to initializing"
        );
        assert!(refresh_by(ctx, &proven, next.revision, || Some(standing())));
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

        assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
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

        let next = claim_supervisor_as(
            ctx,
            holder,
            Some(3),
            None,
            Some(proven.clone()),
            now,
            aterm_slog::Actor::Ctl,
        )
        .expect("a next claim");
        assert!(refresh_by(ctx, &proven, next.revision, || Some(standing())));
        assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
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

        let next = claim_supervisor_as(
            ctx,
            holder,
            Some(4),
            None,
            Some(proven.clone()),
            now,
            aterm_slog::Actor::Ctl,
        )
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
    /// stand-in process (the kept signed `sleep`, its `CODEX_HOME` the
    /// test's directory) holding the session's PROVEN claim, with a PLANTED
    /// record saying `idle` and `draftEmpty: true`, is at its idle point; the
    /// record saying `busy` is not.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn the_idle_point_is_the_proven_processs_own_record_read_live() {
        use crate::session_timeline::{claim_supervisor_as, release_supervisor};
        let dir = aterm_tempfile::TempDir::new().expect("dir");
        let home = std::fs::canonicalize(dir.path()).expect("canonical");
        let sleep = std::fs::canonicalize(aterm_uds::deadline::stand_in(std::path::Path::new(
            "/bin/sleep",
        )))
        .expect("the kept stand-in");
        let sleep_s = sleep.to_string_lossy().into_owned();
        let mut child = aterm_uds::spawnfd::command(&sleep)
            .arg("60")
            .env("CODEX_HOME", &home)
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
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
        claim_supervisor_as(
            ctx,
            &holder,
            Some(1),
            None,
            Some(proven),
            now,
            aterm_slog::Actor::Ctl,
        )
        .expect("a claim");
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
        assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
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
            aterm_slog::Actor::Ctl,
        )
        .expect("claimed again");
        assert!(idle_now(ctx, pid, &start), "positive control, again");
        let _ = child.kill();
        let _ = child.wait();
        assert!(!idle_now(ctx, pid, &start), "a dead identity rests nowhere");
    }

    /// THE READ-ONLY COPY FOLLOWS ITS PROCESS'S OWN RECORD, live: a real stand-in process
    /// ([`stand_in::Sleeper`], its `CODEX_HOME` the test's directory) whose proven claim is
    /// released through the real path keeps its copy read only, and its target taps the thread
    /// the record names, read again at each ask ([`tapped`] over [`self_claimed::read_seen`]): a
    /// `/new` written into the record moves the tapped thread at once and the old thread is held
    /// no more ([`hold_rollout`]); a record naming no thread, or one that says it is another
    /// process's, taps nothing while the target still stands, so the next ask reads it again and
    /// taps the thread it names then. FAIL CLOSED: a record that shows the end coming (`exited`),
    /// or is gone after it was seen (folded to removed), ends the target at that ask, and only a
    /// claim granted and given back again names one. NEGATIVE CONTROLS: another group in front,
    /// and the process ended, name no target; an ended process's record is not read, and its copy
    /// goes ([`discard_dead`]).
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_read_only_copy_follows_its_live_processs_own_record() {
        use crate::session_timeline::{claim_supervisor_as, release_supervisor};
        const THREAD: &str = "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b77";
        const NEXT: &str = "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b78";
        let dir = aterm_tempfile::TempDir::new().expect("dir");
        let home = std::fs::canonicalize(dir.path()).expect("canonical");
        let mut sleeper = stand_in::Sleeper::under(&home);
        let proven = sleeper.proven.clone();
        let pid = proven.pid;
        let group = i32::try_from(pid).expect("a pid");
        sleeper.plant(Some(THREAD));
        let handle = crate::session_store::test_handle_on(7595, -1);
        let ctx = handle.ctx.as_ref();
        let sid = ctx.self_id.as_str();
        let now = crate::metrics::now_us();
        let claim_and_release = || {
            claim_supervisor_as(
                ctx,
                &format!("{SELF_CLAIM_PREFIX}{pid}"),
                Some(1),
                None,
                Some(proven.clone()),
                now,
                aterm_slog::Actor::Ctl,
            )
            .expect("a claim");
            meta_changed(ctx);
            let claimed = rollout_target(ctx).expect("the claim's copy names its thread");
            assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
            assert_eq!(copy_of_tab(sid), None, "no act path is handed the copy");
            let Some(TapTarget::Watched(watched)) = tap_target(ctx, group) else {
                panic!("a read-only target for the released claim's live process");
            };
            (claimed, watched)
        };
        let (claimed, watched) = claim_and_release();
        assert_eq!(
            (claimed.thread.as_str(), claimed.home.as_path()),
            (THREAD, home.as_path())
        );
        assert_eq!(
            (watched.pid, watched.start.as_str(), watched.home.as_path()),
            (pid, proven.start.as_str(), home.as_path())
        );
        let target = TapTarget::Watched(watched);
        let thread = |target: &TapTarget| {
            tapped(ctx, target, self_claimed::read_seen).map(|tapped| tapped.thread)
        };
        let old = tapped(ctx, &target, self_claimed::read_seen).expect("its thread");
        assert_eq!(old.thread, THREAD);
        assert!(hold_rollout(ctx, &old).is_some());
        sleeper.plant(Some(NEXT));
        assert_eq!(
            thread(&target),
            Ok(NEXT.to_string()),
            "a /new under Off taps the new thread"
        );
        assert!(hold_rollout(ctx, &old).is_none(), "never the old one");
        sleeper.plant(None);
        assert_eq!(thread(&target), Err(Untapped::Unnamed));
        assert_eq!(
            tap_target(ctx, group),
            Some(target.clone()),
            "the target stands, so the next ask reads the record again"
        );
        sleeper.plant(Some(NEXT));
        assert_eq!(thread(&target), Ok(NEXT.to_string()));
        sleeper.plant_as(pid + 1, Some(THREAD), "busy");
        assert_eq!(
            thread(&target),
            Err(Untapped::Unnamed),
            "a record that says it is another process's names no thread of this one"
        );
        assert_eq!(tap_target(ctx, group), Some(target.clone()));
        sleeper.plant(Some(NEXT));
        assert_eq!(thread(&target), Ok(NEXT.to_string()));

        sleeper.unplant();
        assert_eq!(
            thread(&target),
            Err(Untapped::Moved),
            "a record gone after it was seen reads removed: the end"
        );
        assert_eq!(tap_target(ctx, group), None);
        sleeper.plant(Some(NEXT));
        assert_eq!(
            thread(&target),
            Err(Untapped::Moved),
            "a record written again names no target until a claim reads it"
        );
        let (_, watched) = claim_and_release();
        let target = TapTarget::Watched(watched);
        assert_eq!(thread(&target), Ok(NEXT.to_string()));
        sleeper.plant_as(pid, Some(NEXT), "exited");
        assert_eq!(
            thread(&target),
            Err(Untapped::Moved),
            "a record that shows the end coming ends the target"
        );
        assert_eq!(tap_target(ctx, group), None);

        sleeper.plant(Some(NEXT));
        let (_, watched) = claim_and_release();
        let target = TapTarget::Watched(watched);
        assert_eq!(thread(&target), Ok(NEXT.to_string()));
        assert_eq!(tap_target(ctx, group + 1), None, "another group in front");
        sleeper.end();
        assert_eq!(tap_target(ctx, group), None, "the process ended");
        assert_eq!(
            thread(&target),
            Err(Untapped::Moved),
            "an ended process's record is not read"
        );
        discard_dead(sid);
        assert!(
            COPIES.lock().unwrap().get(sid).is_none(),
            "a dead process's copy goes"
        );
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
        assert!(!runs_alab_build(me));
        let proven = prove(&holder, facts).expect("this process proves its own claim");
        assert_eq!(proven.pid, me);
        assert!(proven.alive(), "this process runs");
        let reused = Proven {
            start: "Thu Jan  1 00:00:00 1970".to_string(),
            ..proven
        };
        assert!(!reused.alive(), "a reused pid is not the proof's process");
    }
}
