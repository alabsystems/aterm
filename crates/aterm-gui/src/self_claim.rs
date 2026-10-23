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
static COPIES: Mutex<Copies> = Mutex::new(Copies::new());

/// The copies by tab (a `BTreeMap`, so the static needs no lazy init).
type Copies = std::collections::BTreeMap<String, Copy>;

/// The line the crash rule last typed in each tab ([`typed`]): what a
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
        if before.and_then(|copy| copy.resume.ok()) != line {
            crate::harness_host::ring();
        }
    }
    true
}

/// Tab `sid`'s live proven claim, read under its meta guard (a clone; the
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

/// Return the kept session-record copy for tab `sid`.
#[must_use]
pub(crate) fn copy_of_tab(sid: &str) -> Option<Copy> {
    COPIES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(sid)
        .cloned()
}

/// Rollout tap target for a proven `codex-alab` session.
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
        copies.get(ctx.self_id.as_str()),
    )
}

/// Guard holding `meta` and `copies` locks across a bounded timeline hold.
pub(crate) struct RolloutGuard<'a> {
    _meta: std::sync::MutexGuard<'a, crate::session_timeline::SessionMeta>,
    _copies: std::sync::MutexGuard<'static, Copies>,
}

pub(crate) fn hold_rollout<'a>(
    ctx: &'a crate::SessionCtx,
    target: &RolloutTarget,
) -> Option<RolloutGuard<'a>> {
    let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
    let copies = COPIES.lock().unwrap_or_else(PoisonError::into_inner);
    let current = target_of(
        meta.live_claim(crate::metrics::now_us())
            .and_then(|claim| claim.proven.as_ref()),
        meta.supervisor_revision,
        copies.get(ctx.self_id.as_str()),
    );
    (current.as_ref() == Some(target)).then_some(RolloutGuard {
        _meta: meta,
        _copies: copies,
    })
}

/// Take and remove the kept record copy for `(pid, start)` in tab `sid`.
pub(crate) fn take_copy(sid: &str, pid: u32, start: &str) {
    let mut copies = COPIES.lock().unwrap_or_else(PoisonError::into_inner);
    if copies
        .get(sid)
        .is_some_and(|copy| copy.pid == pid && copy.start == start)
    {
        copies.remove(sid);
    }
}

/// Drop tab `sid`'s kept record copy if its process has ended.
pub(crate) fn discard_dead(sid: &str) {
    let dead = copy_of_tab(sid).filter(|copy| !restart_stamp::alive_as(copy.pid, &copy.start));
    if let Some(copy) = dead {
        take_copy(sid, copy.pid, &copy.start);
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

/// Refresh or clear the record copy when a proven claim is released on `ctx` and notify the host.
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
        crate::harness_host::ring();
    }
    true
}

/// Stand-in child process helper for self-claim tests.
#[cfg(test)]
pub(crate) mod stand_in {
    use super::Proven;

    /// What tells [`a_stand_in_agent_when_asked`] to stand in.
    const STAND_IN_ENV: &str = "ATERM_SELF_CLAIM_TEST_STAND_IN";

    /// THE STAND-IN'S BODY: this test binary, which waits for its parent's
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
            initializing: false,
        })
    }

    /// THE ROLLOUT TAP'S CLAIM GATE: only a
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

    /// Only a usage wall, only with the policy in effect and `resume_limits`
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
            assert!(copy_of_tab(sid).is_none(), "passive handback keeps no copy");
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
