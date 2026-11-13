// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Soft `RLIMIT_NOFILE` management across self-updates (launched, fork, and cold lanes) and shell
//! spawns.

use crate::handoff_env::HandoffName;

/// The lane a successor's sessions arrive on, for the record its raise leaves
/// ([`settle_incoming_descriptor_limit`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SuccessorLane {
    /// Over the rendezvous, to a LaunchServices-launched successor.
    Launched,
    /// By inheritance, to a successor its predecessor forked.
    Forked,
}

/// The one place a launch settles its soft descriptor limit: a successor's for the sessions it
/// adopts, any other launch's for the tabs it opens ([`raise_descriptor_limit_for_tabs`]).
pub(crate) fn settle_incoming_descriptor_limit(env: &crate::handoff_env::HandoffEnv) {
    if let Some(limit) = adopt_carried_launcher_limit(env) {
        aterm_log::info!(
            "descriptor limit: new shells get the launcher's soft descriptor limit {limit}, \
             carried by the process that started this one"
        );
    }
    let lane = if env.present(crate::seamless::ENV_RENDEZVOUS)
        || env.present(crate::seamless::ENV_CLAIM)
    {
        SuccessorLane::Launched
    } else if crate::seamless::incoming_offered_in(env) {
        SuccessorLane::Forked
    } else {
        raise_descriptor_limit_for_tabs();
        return;
    };
    match (raise_descriptor_limit_for_claim(), lane) {
        (Ok(Some((from, to))), _) => {
            match lane {
                SuccessorLane::Launched => forget_park_raise(),
                SuccessorLane::Forked => {
                    PARK_RAISED_FROM.store(from, std::sync::atomic::Ordering::Relaxed);
                }
            }
            aterm_log::info!(
                "overlap handoff: raised this successor's soft descriptor limit from {from} to \
                 {to} for the sessions it adopts ({lane:?} lane); new shells keep the \
                 launcher's {}",
                launcher_soft_descriptor_limit().unwrap_or(from)
            );
        }
        (Ok(None), SuccessorLane::Launched) => forget_park_raise(),
        (Ok(None), SuccessorLane::Forked) => {}
        (Err(error), _) => aterm_log::warn!(
            "overlap handoff: could not raise this successor's soft descriptor limit ({error}); \
             a large handoff may not fit it"
        ),
    }
}

/// A launch that adopts no sessions makes the same room for the tabs it opens: a tab holds several
/// descriptors (8 in v0.138.0), so at launchd's soft limit of 256 (a launch from the Dock) v0.138.0
/// opened 29 tabs and refused the next with `Too many open files`. The raise is recorded as a park's, so the launcher's limit is
/// what its shells get and what an older successor of this process starts at; a limit already at
/// the floor (a cold-lane exec's, which carries its own records) is left with its records.
fn raise_descriptor_limit_for_tabs() {
    match raise_descriptor_limit_for_claim() {
        Ok(Some((from, to))) => {
            PARK_RAISED_FROM.store(from, std::sync::atomic::Ordering::Relaxed);
            aterm_log::info!(
                "descriptor limit: raised this launch's soft descriptor limit from {from} to {to} \
                 for the tabs it opens; new shells keep the launcher's {from}"
            );
        }
        Ok(None) => {}
        Err(error) => aterm_log::warn!(
            "descriptor limit: could not raise this launch's soft descriptor limit ({error}); \
             it opens as many tabs as the launcher's limit holds"
        ),
    }
}

/// The soft limit to raise to from `soft` under `hard`.
pub(crate) fn raised_soft_limit(soft: u64, hard: u64) -> Option<u64> {
    aterm_uds::spawnfd::raised_soft_nofile_target(soft, hard)
}

static LAUNCHER_SOFT_NOFILE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// The soft descriptor limit a shell this process spawns must be handed back.
pub(crate) fn launcher_soft_descriptor_limit() -> Option<u64> {
    LAUNCHER_SOFT_NOFILE.get().copied()
}

static PARK_RAISED_FROM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The soft `RLIMIT_NOFILE` this process would hold had no park raised it; `None` without one.
pub(crate) fn park_raised_from() -> Option<u64> {
    Some(PARK_RAISED_FROM.load(std::sync::atomic::Ordering::Relaxed)).filter(|from| *from > 0)
}

fn forget_park_raise() {
    PARK_RAISED_FROM.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// The launch variable in which a predecessor tells its successor the soft descriptor limit it held
/// before a PARK raised it.
pub(crate) const ENV_PARK_RAISED_FROM: HandoffName =
    HandoffName::new("ATERM_HANDOFF_PARK_RAISED_FROM");

/// The launch variable in which a FORK-lane parent tells its successor the soft descriptor limit.
pub(crate) const ENV_LAUNCHER_NOFILE: HandoffName =
    HandoffName::new("ATERM_HANDOFF_LAUNCHER_NOFILE");

const TAKES_LAUNCHER_NOFILE_FROM_BUILD: u64 = 1_790_807_622;

const TAKES_PARK_RAISED_FROM_BUILD: u64 = 1_791_501_389;

/// Name [`ENV_LAUNCHER_NOFILE`] on a successor's `command` whose image keeps this process's soft
/// descriptor limit — the fork lane's child and the cold lane's `exec` alike.
pub(crate) fn carry_launcher_limit(
    command: &mut aterm_uds::spawnfd::StdCommand,
    target_build: u64,
) {
    command.envs(launcher_limit_environment(target_build));
}

fn launcher_limit_environment(
    target_build: u64,
) -> impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)> {
    let soft = (target_build >= TAKES_LAUNCHER_NOFILE_FROM_BUILD)
        .then(launcher_soft_descriptor_limit)
        .flatten()
        .map(|limit| ENV_LAUNCHER_NOFILE.pair(limit.to_string()));
    let from = (target_build >= TAKES_PARK_RAISED_FROM_BUILD)
        .then(park_raised_from)
        .flatten()
        .map(|limit| ENV_PARK_RAISED_FROM.pair(limit.to_string()));
    [soft, from].into_iter().flatten()
}

/// Removes stray carried descriptor-limit env vars on non-update launches; whether it removed
/// any, for [`note_forgotten_stray_limits`] to say once the window's logger is up.
#[must_use]
pub(crate) fn forget_stray_limits(
    env: &mut crate::handoff_env::HandoffEnv,
    update_launch: bool,
) -> bool {
    if update_launch || !(env.present(ENV_LAUNCHER_NOFILE) || env.present(ENV_PARK_RAISED_FROM)) {
        return false;
    }
    env.remove(ENV_LAUNCHER_NOFILE);
    env.remove(ENV_PARK_RAISED_FROM);
    true
}

/// Log that [`forget_stray_limits`] removed a stray: that runs before the window installs its
/// logger, which drops a line logged before that, so the window calls this once it has.
pub(crate) fn note_forgotten_stray_limits() {
    aterm_log::info!(
        "descriptor limit: this launch is no update's, so the launcher's limit in its \
         environment is a stray an older successor left in a shell; new shells get what this \
         process runs at"
    );
}

fn carried_launcher_limit(carried: Option<&str>, soft: u64) -> Option<u64> {
    let s = carried?;
    if s.is_empty() || s.starts_with('0') || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u64>().ok().filter(|carried| *carried <= soft)
}

/// Record the launcher limit a fork-lane parent ([`ENV_LAUNCHER_NOFILE`]) or the node job
/// ([`ENV_NODE_LAUNCHER_NOFILE`]) carried.
pub(crate) fn adopt_carried_launcher_limit(env: &crate::handoff_env::HandoffEnv) -> Option<u64> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: A valid resource id and an exclusively borrowed, writable out-parameter.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
        return None;
    }
    if let Some(from) =
        carried_launcher_limit(env.string(ENV_PARK_RAISED_FROM).as_deref(), limit.rlim_cur)
    {
        PARK_RAISED_FROM.store(from, std::sync::atomic::Ordering::Relaxed);
    }
    let named = env
        .string(ENV_LAUNCHER_NOFILE)
        .or_else(|| env.string(ENV_NODE_LAUNCHER_NOFILE));
    let carried = carried_launcher_limit(named.as_deref(), limit.rlim_cur)?;
    LAUNCHER_SOFT_NOFILE.set(carried).ok()?;
    Some(carried)
}

/// Raise this process's soft `RLIMIT_NOFILE` to [`raised_soft_limit`]'s answer.
pub(crate) fn raise_descriptor_limit_for_claim() -> std::io::Result<Option<(u64, u64)>> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: A valid resource id and an exclusively borrowed, writable out-parameter.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let (soft, hard) = (limit.rlim_cur, limit.rlim_max);
    let Some(target) = raised_soft_limit(soft, hard) else {
        return Ok(None);
    };
    let raised = libc::rlimit {
        rlim_cur: target,
        rlim_max: limit.rlim_max,
    };
    // SAFETY: A valid resource id and a fully initialized rlimit that keeps the hard limit
    // unchanged.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const raised) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let _ = LAUNCHER_SOFT_NOFILE.set(soft);
    Ok(Some((soft, target)))
}

/// The node job starts at launchd's soft limit of 256, which [`crate::fleet_listen`] shares out as
/// half the fleet slots a window serves: the job that answers every member's reads needs the room
/// no less than a window does.
pub(crate) fn raise_descriptor_limit_for_node() {
    match raise_descriptor_limit_for_claim() {
        Ok(Some((from, to))) => aterm_log::info!(
            "node: raised the soft descriptor limit from {from} to {to} for its fleet slots"
        ),
        Ok(None) => {}
        Err(error) => aterm_log::warn!(
            "node: could not raise the soft descriptor limit ({error}); it serves the fleet slots \
             the launcher's limit holds"
        ),
    }
}

/// The launch variable with which the node job tells a window it launches directly the launcher's
/// soft limit: the window inherits the job's raise ([`raise_descriptor_limit_for_node`]), so it has
/// none of its own to record, and its shells would keep the raise. Unlike
/// [`ENV_LAUNCHER_NOFILE`], which only an update's successor may read, it is meant for a launch that
/// is no update's; the window takes it before any shell can see it.
pub(crate) const ENV_NODE_LAUNCHER_NOFILE: HandoffName =
    HandoffName::new("ATERM_NODE_LAUNCHER_NOFILE");

/// [`ENV_NODE_LAUNCHER_NOFILE`] for a window this process launches directly, where it raised its
/// limit.
pub(crate) fn window_launcher_limit() -> Option<(std::ffi::OsString, std::ffi::OsString)> {
    launcher_soft_descriptor_limit().map(|soft| ENV_NODE_LAUNCHER_NOFILE.pair(soft.to_string()))
}

/// The keeper's recovery needs the same room.
pub(crate) fn raise_descriptor_limit_for_recovery() {
    match raise_descriptor_limit_for_claim() {
        Ok(Some((from, to))) => {
            forget_park_raise();
            aterm_log::info!(
                "keeper: raised the soft descriptor limit from {from} to {to} for the sessions \
                 the boot recovery adopts"
            );
        }
        Ok(None) => forget_park_raise(),
        Err(error) => aterm_log::warn!(
            "keeper: could not raise the soft descriptor limit ({error}); a large recovery may \
             not fit it"
        ),
    }
}

/// Raises this process's soft descriptor limit for the park when `target_build >= build`.
pub(crate) fn raise_descriptor_limit_for_park(target_build: u64, build: u64) {
    if target_build < build {
        aterm_log::info!(
            "update apply: the soft descriptor limit is left as it is for the park — target build \
             {target_build} is older than this build {build} and may not read the launcher's \
             limit it would be told, so its shells would inherit the raise"
        );
        return;
    }
    match raise_descriptor_limit_for_claim() {
        Ok(Some((from, to))) => {
            PARK_RAISED_FROM.store(from, std::sync::atomic::Ordering::Relaxed);
            aterm_log::info!(
                "update apply: raised this process's soft descriptor limit from {from} to {to} \
                 for the park (each session's duplicate master, the rendezvous and the grant); \
                 new shells keep the launcher's {from}"
            );
        }
        Ok(None) => {}
        Err(error) => aterm_log::warn!(
            "update apply: could not raise this process's soft descriptor limit ({error}); a \
             large desk may not fit the park"
        ),
    }
}

/// Pre-park soft descriptor limit to restore when starting an older successor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OlderImageLimit {
    /// The soft limit the image is started at: this process's before the
    /// park's raise.
    soft: u64,
    /// The limits this process runs at, the hard one kept as it is.
    raised: (u64, u64),
    /// One past the highest descriptor number this process can hold.
    table: libc::c_int,
}

fn older_image_soft_limit(
    target_build: u64,
    build: u64,
    park_raised_from: Option<u64>,
    soft: u64,
) -> Option<u64> {
    if target_build >= build {
        return None;
    }
    park_raised_from.filter(|from| *from < soft)
}

impl OlderImageLimit {
    fn toward(target_build: u64, build: u64) -> Option<Self> {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: A valid resource id and an exclusively borrowed, writable out-parameter for the
        // one call.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
            return None;
        }
        let soft = older_image_soft_limit(target_build, build, park_raised_from(), limit.rlim_cur)?;
        Some(Self {
            soft,
            raised: (limit.rlim_cur, limit.rlim_max),
            table: aterm_uds::spawnfd::descriptor_bound(),
        })
    }

    fn inheritable_at_or_above(&self) -> Option<libc::c_int> {
        let floor = libc::c_int::try_from(self.soft).unwrap_or(libc::c_int::MAX);
        (floor..self.table).find(|fd| {
            // SAFETY: `F_GETFD` only reads one number's flags; a number that is not open answers
            // -1.
            let flags = unsafe { libc::fcntl(*fd, libc::F_GETFD) };
            flags >= 0 && flags & libc::FD_CLOEXEC == 0
        })
    }

    fn set_soft(&self, soft: u64) -> std::io::Result<()> {
        let limit = libc::rlimit {
            rlim_cur: soft,
            rlim_max: self.raised.1,
        };
        // SAFETY: A valid resource id and a fully initialised rlimit that keeps the hard limit as
        // it was.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Starts the fork-lane successor `command` at [`OlderImageLimit`] when `target_build < build`.
pub(crate) fn start_older_image_at_its_limit(
    command: &mut aterm_uds::spawnfd::StdCommand,
    target_build: u64,
    build: u64,
    inherited: &[std::os::fd::RawFd],
) {
    use std::os::unix::process::CommandExt as _;
    let Some(limit) = OlderImageLimit::toward(target_build, build) else {
        return;
    };
    let soft = limit.soft;
    if let Some(fd) = inherited
        .iter()
        .copied()
        .filter(|fd| u64::try_from(*fd).is_ok_and(|fd| fd >= soft))
        .max()
        .or_else(|| limit.inheritable_at_or_above())
    {
        aterm_log::warn!(
            "update apply: target build {target_build} is older than this build {build}, and its \
             successor keeps this process's raised soft descriptor limit {}: it would keep \
             descriptor {fd}, at or above the {soft} this process held before the park's raise, \
             so its shells get the raise",
            limit.raised.0
        );
        return;
    }
    aterm_log::info!(
        "update apply: target build {target_build} is older than this build {build}; its \
         successor is started at the soft descriptor limit {soft} this process held before the \
         park's raise"
    );
    // SAFETY: the closure runs between fork and exec and calls only `fcntl` and `setrlimit` over
    // integers it owns — no allocation, no lock.
    unsafe {
        command.pre_exec(move || {
            // A descriptor the image would keep at or above the limit, or a
            // refused `setrlimit` (lowering a soft limit needs no privilege):
            // the child keeps the limit it inherited, and the spawn goes on.
            if limit.inheritable_at_or_above().is_none() {
                let _ = limit.set_soft(limit.soft);
            }
            Ok(())
        });
    }
}

/// [`OlderImageLimit`] for the cold lane's `exec`, which replaces this process.
pub(crate) fn lower_for_older_image(target_build: u64, build: u64) -> Option<LoweredForOlderImage> {
    let limit = OlderImageLimit::toward(target_build, build)?;
    let soft = limit.soft;
    if let Some(fd) = limit.inheritable_at_or_above() {
        aterm_log::warn!(
            "update apply: target build {target_build} is older than this build {build}, and the \
             new image keeps this process's raised soft descriptor limit {}: descriptor {fd}, \
             which the exec keeps, is at or above the {soft} this process held before the \
             park's raise",
            limit.raised.0
        );
        return None;
    }
    if let Err(error) = limit.set_soft(soft) {
        aterm_log::warn!(
            "update apply: target build {target_build} is older than this build {build}, and the \
             soft descriptor limit could not be lowered to {soft} for its exec ({error})"
        );
        return None;
    }
    aterm_log::info!(
        "update apply: target build {target_build} is older than this build {build}; its image \
         is started at the soft descriptor limit {soft} this process held before the park's \
         raise"
    );
    Some(LoweredForOlderImage { limit })
}

/// See [`lower_for_older_image`].
pub(crate) struct LoweredForOlderImage {
    limit: OlderImageLimit,
}

impl Drop for LoweredForOlderImage {
    fn drop(&mut self) {
        let raised = self.limit.raised.0;
        if let Err(error) = self.limit.set_soft(raised) {
            aterm_log::warn!(
                "update apply: the exec returned, and the soft descriptor limit could not be \
                 raised back to {raised} ({error})"
            );
        }
    }
}

#[cfg(test)]
#[path = "descriptor_limit_tests.rs"]
pub(crate) mod tests;
