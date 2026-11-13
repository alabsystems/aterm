// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kernel-proven `codex-alab@<pid>` self-claim verification and live session-record copies.

use std::sync::{Mutex, PoisonError};

use aterm_agent::harness::self_claimed::{self, Copy};
use aterm_agent::harness::{agent_image, restart_stamp};

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
        .is_some_and(|exe| {
            agent_image::in_vendor_layout(
                aterm_phase::Program::Codex,
                &agent_image::Image::at(&exe),
                &agent_image::Places::default(),
            )
        })
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

/// The line the crash rule last typed in tab `sid` ([`typed`]).
pub(crate) fn typed_for(sid: &str) -> Option<self_claimed::Resume> {
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

/// BOOK A FACT A PROVEN SELF-CLAIM'S ACT CARRIED IN ITS SESSION'S LEDGER (`harness fact told`,
/// gap 23): `fact`, told by `holder`'s act on session `sid`'s proven claim at `t_ms`, as one
/// [`aterm_agent::supervise::approvals::SelfClaimFactRow`] in the ledger under `state` (the
/// state directory; `None` where there is none), through the ledger's one append under the lock
/// its cut holds ([`aterm_agent::supervise::approvals::book_self_claim_fact`]): aterm's own
/// `fact-<kind>@v1` `typed` row with the claim's holder, which a successor seeds told and Settings
/// ▸ Harness counts as the holder's. Blocking file I/O on the control lane that took the ack,
/// with no session lock held; whether the row was written (a failure is logged, and the told
/// mark stands in memory).
pub(crate) fn book_fact(
    state: Option<&std::path::Path>,
    sid: &str,
    holder: &str,
    fact: &aterm_agent::supervise::Fact,
    t_ms: i64,
) -> bool {
    use aterm_agent::supervise::approvals;
    let kind = fact.key.kind.word();
    let Some(path) = state.and_then(|root| approvals::path_under(root, Some(sid))) else {
        aterm_log::info!(
            "self-claim @{sid}: fact {kind} {:016x}: no state directory to book it in",
            fact.key.id
        );
        return false;
    };
    let mut refused = Vec::new();
    let written = approvals::book_self_claim_fact(
        &path,
        Some(sid),
        t_ms,
        &approvals::SelfClaimFactRow { holder, fact },
        &mut refused,
    )
    .is_some();
    if !written {
        aterm_log::info!(
            "self-claim @{sid}: fact {kind} {:016x}: not booked: {}",
            fact.key.id,
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
#[path = "self_claim_stand_in_tests.rs"]
pub(crate) mod stand_in;

#[cfg(test)]
#[path = "self_claim_tests.rs"]
mod tests;
