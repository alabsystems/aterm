// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Soft `RLIMIT_NOFILE` management across self-updates (launched, fork, and cold lanes) and shell
//! spawns.

use crate::handoff_env::HandoffName;

/// The soft `RLIMIT_NOFILE` a launched successor raises itself to before it
/// dials (never above its hard limit).
const SUCCESSOR_DESCRIPTOR_FLOOR: u64 = 4096;

/// The lane a successor's sessions arrive on, for the record its raise leaves
/// ([`settle_incoming_descriptor_limit`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SuccessorLane {
    /// Over the rendezvous, to a LaunchServices-launched successor.
    Launched,
    /// By inheritance, to a successor its predecessor forked.
    Forked,
}

/// The one place a successor settles its soft descriptor limit.
pub(crate) fn settle_incoming_descriptor_limit(env: &crate::handoff_env::HandoffEnv) {
    if let Some(limit) = adopt_carried_launcher_limit(env) {
        aterm_log::info!(
            "overlap handoff: new shells get the launcher's soft descriptor limit {limit}, \
             carried by the outgoing process"
        );
    }
    let lane = if env.present(crate::seamless::ENV_RENDEZVOUS)
        || env.present(crate::seamless::ENV_CLAIM)
    {
        SuccessorLane::Launched
    } else if crate::seamless::incoming_offered_in(env) {
        SuccessorLane::Forked
    } else {
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

/// The soft limit to raise to from `soft` under `hard`.
pub(crate) fn raised_soft_limit(soft: u64, hard: u64) -> Option<u64> {
    let target = SUCCESSOR_DESCRIPTOR_FLOOR.min(hard);
    (target > soft).then_some(target)
}

/// The soft `RLIMIT_NOFILE` this process was LAUNCHED with, recorded by the
/// first [`raise_descriptor_limit_for_claim`] that moved it — a claim's, a
/// park's or a recovery's — or adopted from a predecessor that carried it
/// ([`adopt_carried_launcher_limit`]); unset when the limit was never raised
/// (a Dock launch that has not yet attempted an update, a limit that already
/// had room).
static LAUNCHER_SOFT_NOFILE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// The soft descriptor limit a shell this process spawns must be handed back.
pub(crate) fn launcher_soft_descriptor_limit() -> Option<u64> {
    LAUNCHER_SOFT_NOFILE.get().copied()
}

/// The soft `RLIMIT_NOFILE` this process would hold had no PARK raised it;
/// 0 when it holds no park's raise.
static PARK_RAISED_FROM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// See [`PARK_RAISED_FROM`].
pub(crate) fn park_raised_from() -> Option<u64> {
    Some(PARK_RAISED_FROM.load(std::sync::atomic::Ordering::Relaxed)).filter(|from| *from > 0)
}

/// The raise this process now holds is one a claim or a recovery makes
/// whatever a park did before it (see [`PARK_RAISED_FROM`]).
fn forget_park_raise() {
    PARK_RAISED_FROM.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// The launch variable in which a predecessor tells its successor the soft descriptor limit it.
pub(crate) const ENV_PARK_RAISED_FROM: HandoffName =
    HandoffName::new("ATERM_HANDOFF_PARK_RAISED_FROM");

/// The launch variable in which a FORK-lane parent tells its successor the soft descriptor limit.
pub(crate) const ENV_LAUNCHER_NOFILE: HandoffName =
    HandoffName::new("ATERM_HANDOFF_LAUNCHER_NOFILE");

/// v0.100.0's build (`RELEASES.ledger`; no v0.99.0 was cut): the first
/// release that takes [`ENV_LAUNCHER_NOFILE`] out of its environment at its
/// capture (06b06e74e).
const TAKES_LAUNCHER_NOFILE_FROM_BUILD: u64 = 1_790_807_622;

/// v0.101.0's build: the first release that takes [`ENV_PARK_RAISED_FROM`]
/// out of its environment (ddaf83ae2f, after v0.100.0 was cut).
const TAKES_PARK_RAISED_FROM_BUILD: u64 = 1_791_501_389;

/// Name [`ENV_LAUNCHER_NOFILE`] on a successor's `command` whose image keeps this process's soft.
pub(crate) fn carry_launcher_limit(
    command: &mut aterm_uds::spawnfd::StdCommand,
    target_build: u64,
) {
    if target_build < TAKES_LAUNCHER_NOFILE_FROM_BUILD {
        return;
    }
    if let Some(soft) = launcher_soft_descriptor_limit() {
        command.envs([ENV_LAUNCHER_NOFILE.pair(soft.to_string())]);
    }
    if target_build < TAKES_PARK_RAISED_FROM_BUILD {
        return;
    }
    if let Some(from) = park_raised_from() {
        command.envs([ENV_PARK_RAISED_FROM.pair(from.to_string())]);
    }
}

/// A carried limit is an update's word to its successor, and nothing ELSE's .
pub(crate) fn forget_stray_limits(env: &mut crate::handoff_env::HandoffEnv, update_launch: bool) {
    if update_launch || !(env.present(ENV_LAUNCHER_NOFILE) || env.present(ENV_PARK_RAISED_FROM)) {
        return;
    }
    env.remove(ENV_LAUNCHER_NOFILE);
    env.remove(ENV_PARK_RAISED_FROM);
    aterm_log::info!(
        "descriptor limit: this launch is no update's, so the launcher's limit in its \
         environment is a stray an older successor left in a shell; new shells get what this \
         process runs at"
    );
}

/// A carried launcher limit this process may adopt: a number, and never above
/// the soft limit it runs at (`soft`) — it only ever gives the shells LESS
/// than this process has, which is the one direction the raise moved.
fn carried_launcher_limit(carried: Option<&str>, soft: u64) -> Option<u64> {
    carried?
        .parse::<u64>()
        .ok()
        .filter(|carried| *carried > 0 && *carried <= soft)
}

/// Record the launcher limit a fork-lane parent carried ([`ENV_LAUNCHER_NOFILE`]).
pub(crate) fn adopt_carried_launcher_limit(env: &crate::handoff_env::HandoffEnv) -> Option<u64> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: A valid resource id and an exclusively borrowed, writable out-parameter for the one.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
        return None;
    }
    if let Some(from) =
        carried_launcher_limit(env.string(ENV_PARK_RAISED_FROM).as_deref(), limit.rlim_cur)
    {
        PARK_RAISED_FROM.store(from, std::sync::atomic::Ordering::Relaxed);
    }
    let carried =
        carried_launcher_limit(env.string(ENV_LAUNCHER_NOFILE).as_deref(), limit.rlim_cur)?;
    LAUNCHER_SOFT_NOFILE.set(carried).ok()?;
    Some(carried)
}

/// Raise this process's soft `RLIMIT_NOFILE` to [`raised_soft_limit`]'s answer.
pub(crate) fn raise_descriptor_limit_for_claim() -> std::io::Result<Option<(u64, u64)>> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: A valid resource id and an exclusively borrowed, writable out-parameter for the one.
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
    // SAFETY: A valid resource id and a fully initialised rlimit that keeps the hard limit as it.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const raised) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let _ = LAUNCHER_SOFT_NOFILE.set(soft);
    Ok(Some((soft, target)))
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

/// The outgoing process needs the same room at its park (#3).
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

/// The limit an older successor is started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OlderImageLimit {
    /// The soft limit the image is started.
    soft: u64,
    /// The limits this process runs at, the hard one kept as it.
    raised: (u64, u64),
    /// One past the highest descriptor number this process can hold.
    table: libc::c_int,
}

/// The soft limit an image of `target_build`, started by a process of `build`
/// that runs at `soft`, is started at — `None` to leave it the limit it
/// inherits: only an older target, and only under a park's raise.
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
    /// What an image of `target_build` is started at by this process of
    /// `build` ([`older_image_soft_limit`] over this process's limits).
    fn toward(target_build: u64, build: u64) -> Option<Self> {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: A valid resource id and an exclusively borrowed, writable out-parameter.
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

    /// The first descriptor at or above [`Self::soft`] that an `exec` keeps:
    /// open, and not close-on-exec.
    fn inheritable_at_or_above(&self) -> Option<libc::c_int> {
        let floor = libc::c_int::try_from(self.soft).unwrap_or(libc::c_int::MAX);
        (floor..self.table).find(|fd| {
            // SAFETY: `F_GETFD` only reads one number's flags.
            let flags = unsafe { libc::fcntl(*fd, libc::F_GETFD) };
            flags >= 0 && flags & libc::FD_CLOEXEC == 0
        })
    }

    /// Set this process's soft limit to `soft`, the hard one as it is. A
    /// bare `setrlimit` — async-signal-safe, as the PTY's own post-fork child
    /// relies on.
    fn set_soft(&self, soft: u64) -> std::io::Result<()> {
        let limit = libc::rlimit {
            rlim_cur: soft,
            rlim_max: self.raised.1,
        };
        // SAFETY: A valid resource id and a fully initialised rlimit that keeps the hard limit.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Start the FORK lane's successor `command` at [`OlderImageLimit`] when its `target_build`.
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
    // SAFETY: The closure runs between fork and exec and calls only `fcntl` and `setrlimit`.
    unsafe {
        command.pre_exec(move || {
            // A descriptor the image would keep at or above the limit, or a
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
pub(crate) mod tests {
    use super::*;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

    fn pipe_for_test() -> (OwnedFd, OwnedFd) {
        let mut raw = [0i32; 2];
        // SAFETY: `pipe` fills two descriptor numbers into the array.
        assert_eq!(unsafe { libc::pipe(raw.as_mut_ptr()) }, 0, "pipe");
        // SAFETY: Fresh pipe descriptors, exclusively owned from here.
        unsafe { (OwnedFd::from_raw_fd(raw[0]), OwnedFd::from_raw_fd(raw[1])) }
    }

    /// THE LAUNCHER'S LIMIT OUTLIVES A FORK-LANE HOP.
    #[test]
    fn a_forked_successor_adopts_the_carried_launcher_limit() {
        assert_eq!(carried_launcher_limit(Some("256"), 4096), Some(256));
        assert_eq!(carried_launcher_limit(Some("4096"), 4096), Some(4096));
        assert_eq!(
            carried_launcher_limit(Some("8192"), 4096),
            None,
            "never raised by it"
        );
        assert_eq!(carried_launcher_limit(Some("0"), 4096), None);
        assert_eq!(carried_launcher_limit(Some("-1"), 4096), None);
        assert_eq!(carried_launcher_limit(Some("lots"), 4096), None);
        assert_eq!(carried_launcher_limit(None, 4096), None, "an older parent");
    }

    /// THE LAUNCHER'S LIMIT OUTLIVES A COLD-LANE EXEC TOO (#81).
    #[test]
    fn a_cold_lane_exec_carries_the_launchers_limit_to_the_new_images_shells() {
        const NEW_IMAGE: &str = "ATERM_TEST_COLD_EXEC_NEW_IMAGE";
        if std::env::var_os(NEW_IMAGE).is_some() {
            let env = crate::handoff_env::HandoffEnv::capture();
            let carried =
                adopt_carried_launcher_limit(&env).expect("the new image adopts the carried limit");
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: A valid resource id and a writable out-parameter.
            assert_eq!(
                unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) },
                0
            );
            assert!(
                carried < limit.rlim_cur,
                "the raise crossed the exec: {carried} < {}",
                limit.rlim_cur
            );
            a_shell_spawned_after_the_raise_gets_the_launchers_limit(carried, limit.rlim_cur);
            return;
        }

        if !crate::control_auth::enter_low_nofile_test_child(64) {
            return;
        }
        let (from, _) = raise_descriptor_limit_for_claim()
            .expect("the raise is permitted")
            .expect("a 64-spare limit is below the floor");
        let mut image = crate::test_alone::StandIn::this_test();
        image.command().env(NEW_IMAGE, "1");
        carry_launcher_limit(image.command(), TAKES_PARK_RAISED_FROM_BUILD);
        assert!(
            image
                .command()
                .get_envs()
                .any(|(key, value)| key == ENV_LAUNCHER_NOFILE.name()
                    && value == Some(std::ffi::OsStr::new(&from.to_string()))),
            "the command names the launcher's {from}"
        );
        image.spawn().passed();
    }

    /// This process's soft descriptor limit.
    fn soft_nofile() -> u64 {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: A valid resource id and a writable out-parameter.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) },
            0
        );
        limit.rlim_cur
    }

    /// Mark every descriptor at or above `floor` close-on-exec: what a busy
    /// test run leaked into a child of the test binary there is not the
    /// desk's, and a window at that limit could hold none.
    fn mark_close_on_exec_at_or_above(floor: u64) {
        let floor = i32::try_from(floor).expect("a descriptor number");
        for fd in floor..aterm_uds::spawnfd::descriptor_bound() {
            // SAFETY: F_GETFD/F_SETFD on any number only reads or sets its close-on-exec flag.
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags >= 0 {
                    libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
                }
            }
        }
    }

    /// The soft descriptor limit a fork-lane successor of `target_build`,
    /// started by this process of `build` and handed `inherited`, runs at —
    /// read by a stand-in's own `ulimit -n`, through the lane's own hook
    /// ([`start_older_image_at_its_limit`]).
    fn successor_soft_nofile(
        target_build: u64,
        build: u64,
        inherited: &[std::os::fd::RawFd],
    ) -> u64 {
        let mut successor = std::process::Command::new("/bin/sh");
        successor.args(["-c", "ulimit -n"]);
        start_older_image_at_its_limit(&mut successor, target_build, build, inherited);
        let out = successor.output().expect("run the successor's stand-in");
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("a limit")
    }

    /// AN OLDER SUCCESSOR STARTS AT THE LIMIT HELD BEFORE A PARK'S RAISE.
    #[test]
    fn an_older_successor_starts_at_the_limit_held_before_a_parks_raise() {
        const NEW_IMAGE: &str = "ATERM_TEST_PARK_RAISE_NEW_IMAGE";
        const BUILD: u64 = 7;

        assert_eq!(older_image_soft_limit(6, 7, Some(256), 4096), Some(256));
        assert_eq!(
            older_image_soft_limit(7, 7, Some(256), 4096),
            None,
            "the same build reads the told limit"
        );
        assert_eq!(
            older_image_soft_limit(8, 7, Some(256), 4096),
            None,
            "a newer one does too"
        );
        assert_eq!(
            older_image_soft_limit(6, 7, None, 4096),
            None,
            "no park's raise: inherited as before round eight"
        );
        assert_eq!(
            older_image_soft_limit(6, 7, Some(4096), 4096),
            None,
            "nothing to lower"
        );

        if std::env::var_os(NEW_IMAGE).is_some() {
            let env = crate::handoff_env::HandoffEnv::capture();
            let launcher =
                adopt_carried_launcher_limit(&env).expect("the launcher's limit is carried");
            assert_eq!(
                park_raised_from(),
                Some(launcher),
                "the park's raise is adopted"
            );
            assert!(launcher < soft_nofile(), "the raise crossed");
            mark_close_on_exec_at_or_above(launcher);
            assert_eq!(successor_soft_nofile(BUILD - 1, BUILD, &[]), launcher);
            return;
        }

        if !crate::control_auth::enter_low_nofile_test_child(64) {
            return;
        }
        let launchd = soft_nofile();
        mark_close_on_exec_at_or_above(launchd);
        assert_eq!(OlderImageLimit::toward(BUILD - 1, BUILD), None);
        assert_eq!(successor_soft_nofile(BUILD - 1, BUILD, &[]), launchd);
        raise_descriptor_limit_for_park(BUILD - 1, BUILD);
        assert_eq!(soft_nofile(), launchd, "no raise toward an older target");
        raise_descriptor_limit_for_park(BUILD, BUILD);
        let raised = soft_nofile();
        assert!(raised > launchd, "the park's raise is made: {raised}");
        assert_eq!(
            park_raised_from(),
            Some(launchd),
            "…and recorded as a park's"
        );

        assert_eq!(
            successor_soft_nofile(BUILD - 1, BUILD, &[]),
            launchd,
            "the fork lane starts the older successor at launchd's limit, not {raised}"
        );
        assert_eq!(
            successor_soft_nofile(BUILD, BUILD, &[]),
            raised,
            "a same-or-newer successor inherits the room, and reads the told limit"
        );

        let (read, _write) = pipe_for_test();
        let launchd_fd = i32::try_from(launchd).expect("a descriptor number");
        // SAFETY: F_DUPFD duplicates a live descriptor at the first free number at or above.
        let high = unsafe { libc::fcntl(read.as_raw_fd(), libc::F_DUPFD, launchd_fd) };
        assert!(
            high >= launchd_fd,
            "PRECONDITION: room above launchd's limit"
        );
        // SAFETY: A fresh duplicate, owned here from now.
        let high = unsafe { OwnedFd::from_raw_fd(high) };
        assert_eq!(
            successor_soft_nofile(BUILD - 1, BUILD, &[high.as_raw_fd()]),
            raised,
            "handed above the limit"
        );
        assert_eq!(
            successor_soft_nofile(BUILD - 1, BUILD, &[]),
            raised,
            "a stray the exec would keep"
        );
        assert!(
            lower_for_older_image(BUILD - 1, BUILD).is_none(),
            "the cold lane's exec would keep it too"
        );
        assert_eq!(soft_nofile(), raised);
        drop(high);

        let lowered = lower_for_older_image(BUILD - 1, BUILD).expect("lowered for the exec");
        assert_eq!(
            soft_nofile(),
            launchd,
            "the older image starts at launchd's limit"
        );
        drop(lowered);
        assert_eq!(soft_nofile(), raised, "raised back when the exec returned");
        assert!(
            lower_for_older_image(BUILD, BUILD).is_none(),
            "never toward this build"
        );

        let mut image = crate::test_alone::StandIn::this_test();
        image.command().env(NEW_IMAGE, "1");
        carry_launcher_limit(image.command(), TAKES_PARK_RAISED_FROM_BUILD);
        assert!(
            image
                .command()
                .get_envs()
                .any(|(key, value)| key == ENV_PARK_RAISED_FROM.name()
                    && value == Some(std::ffi::OsStr::new(&launchd.to_string()))),
            "the command names the park's raise from {launchd}"
        );
        image.spawn().passed();

        raise_descriptor_limit_for_recovery();
        assert_eq!(park_raised_from(), None, "the raise is a recovery's now");
        assert_eq!(successor_soft_nofile(BUILD - 1, BUILD, &[]), raised);
    }

    /// …AND THE RAISE STAYS THE SUCCESSOR'S.
    pub(crate) fn a_shell_spawned_after_the_raise_gets_the_launchers_limit(from: u64, to: u64) {
        use std::os::unix::process::CommandExt as _;
        assert_eq!(
            launcher_soft_descriptor_limit(),
            Some(from),
            "the raise was recorded"
        );
        let limits = crate::spawn::daily_driver_limits();
        assert_eq!(limits.open_files, Some(from));
        // SAFETY: Test-only root authority, used for this one child's cap.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let mut shell = std::process::Command::new("/bin/sh");
        shell.args(["-c", "ulimit -n"]);
        // SAFETY: `apply` is a bare `getrlimit`/`setrlimit` loop with a valid cap.
        unsafe {
            shell.pre_exec(move || limits.apply(&cap));
        }
        let out = shell.output().expect("run the shell");
        assert!(out.status.success(), "{out:?}");
        let seen = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(
            seen,
            from.to_string(),
            "the shell sees the launcher's limit, not {to}"
        );
    }

    /// EVERY LANE SETTLES ITS DESCRIPTOR LIMIT IN ONE PLACE (
    /// U9).
    #[test]
    fn every_lane_settles_its_descriptor_limit_in_one_place() {
        fn soft_limit() -> u64 {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: A valid resource id and a writable out-parameter.
            assert_eq!(
                unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) },
                0
            );
            limit.rlim_cur
        }
        if !crate::control_auth::enter_low_nofile_test_child(64) {
            return;
        }
        let low = soft_limit();
        settle_incoming_descriptor_limit(&crate::handoff_env::HandoffEnv::default());
        assert_eq!(soft_limit(), low, "a cold launch raises nothing");
        assert_eq!(launcher_soft_descriptor_limit(), None);

        PARK_RAISED_FROM.store(low, std::sync::atomic::Ordering::Relaxed);
        let mut launched = crate::handoff_env::HandoffEnv::default();
        launched.set(crate::seamless::ENV_RENDEZVOUS, "/nonexistent/rendezvous");
        settle_incoming_descriptor_limit(&launched);
        assert!(
            soft_limit() > low,
            "a launched successor makes room before it dials"
        );
        assert_eq!(launcher_soft_descriptor_limit(), Some(low));
        assert_eq!(
            park_raised_from(),
            None,
            "a launched successor's raise is a claim's"
        );

        let raised = soft_limit();
        let mut forked = crate::handoff_env::HandoffEnv::default();
        forked.set(crate::seamless::ENV_FDS, "3,4");
        settle_incoming_descriptor_limit(&forked);
        assert_eq!(
            soft_limit(),
            raised,
            "room already held is not raised again"
        );
        assert_eq!(park_raised_from(), None, "and the record stays the claim's");
    }

    /// A STRAY LAUNCHER LIMIT IS NOBODY'S. A window
    /// whose limit a park raised updates; its successor is a build that never
    /// reads the names (every release up to v0.99.0, reached by a rollback,
    /// played here by a stand-in that takes neither) and so leaves them in the
    /// environment of every shell it opens, while it takes `ATERM_UPDATED_FROM`
    /// out as every build does.
    #[test]
    fn only_an_updates_launch_adopts_a_carried_launcher_limit() {
        const ROLE: &str = "ATERM_TEST_STRAY_LIMIT_ROLE";
        const BUILD: u64 = 7;
        /// `main_entry`'s steps for the limit, in its order.
        fn launch() -> crate::handoff_env::HandoffEnv {
            let mut env = crate::handoff_env::HandoffEnv::capture();
            forget_stray_limits(&mut env, std::env::var_os("ATERM_UPDATED_FROM").is_some());
            settle_incoming_descriptor_limit(&env);
            env
        }
        match std::env::var(ROLE).as_deref() {
            Ok("older successor") => {
                let mut window = crate::test_alone::StandIn::this_test();
                window
                    .command()
                    .env(ROLE, "window")
                    .env_remove("ATERM_UPDATED_FROM");
                window.spawn().passed();
                launch();
                let launcher = launcher_soft_descriptor_limit().expect("the update's word");
                assert!(launcher < soft_nofile(), "below the raise it inherited");
                assert_eq!(park_raised_from(), Some(launcher));
                return;
            }
            Ok("window") => {
                let env = launch();
                assert_eq!(
                    launcher_soft_descriptor_limit(),
                    None,
                    "a window started from a shell takes no stray launcher limit"
                );
                assert_eq!(park_raised_from(), None, "nor a stray park's raise");
                let mut reexec = crate::test_alone::StandIn::this_test();
                reexec
                    .command()
                    .env(ROLE, "boot re-exec")
                    .env("ATERM_UPDATED_FROM", BUILD.to_string())
                    .envs(env.exec_env());
                reexec.spawn().passed();
                return;
            }
            Ok("boot re-exec") => {
                launch();
                assert_eq!(
                    launcher_soft_descriptor_limit(),
                    None,
                    "the boot re-exec carries no stray on"
                );
                assert_eq!(park_raised_from(), None);
                return;
            }
            _ => {}
        }
        if !crate::control_auth::enter_low_nofile_test_child(64) {
            return;
        }
        raise_descriptor_limit_for_park(BUILD, BUILD);
        assert!(park_raised_from().is_some(), "PRECONDITION: a park's raise");
        let mut successor = crate::test_alone::StandIn::this_test();
        successor
            .command()
            .env(ROLE, "older successor")
            .env("ATERM_UPDATED_FROM", BUILD.to_string());
        carry_launcher_limit(successor.command(), TAKES_PARK_RAISED_FROM_BUILD);
        successor.spawn().passed();
    }

    /// ONLY A TARGET THAT TAKES THE NAMES IS TOLD THEM:
    /// a successor built before a name leaves it in its shells' environment.
    #[test]
    fn only_a_target_that_takes_the_names_is_told_them() {
        const V0_98_0: u64 = 1_790_607_299;
        if !crate::control_auth::enter_low_nofile_test_child(64) {
            return;
        }
        raise_descriptor_limit_for_park(7, 7);
        let launchd = park_raised_from().expect("PRECONDITION: a park's raise");
        assert_eq!(launcher_soft_descriptor_limit(), Some(launchd));
        let told = |target_build: u64| -> Vec<&'static str> {
            let mut command = std::process::Command::new("/usr/bin/true");
            carry_launcher_limit(&mut command, target_build);
            [ENV_LAUNCHER_NOFILE, ENV_PARK_RAISED_FROM]
                .into_iter()
                .filter(|name| {
                    command.get_envs().any(|(key, value)| {
                        key == name.name()
                            && value == Some(std::ffi::OsStr::new(&launchd.to_string()))
                    })
                })
                .map(HandoffName::name)
                .collect()
        };
        assert_eq!(told(V0_98_0), Vec::<&str>::new(), "v0.98.0 takes neither");
        assert_eq!(
            told(TAKES_LAUNCHER_NOFILE_FROM_BUILD - 1),
            Vec::<&str>::new()
        );
        assert_eq!(
            told(TAKES_LAUNCHER_NOFILE_FROM_BUILD),
            vec![ENV_LAUNCHER_NOFILE.name()],
            "v0.100.0 takes the launcher's limit alone"
        );
        assert_eq!(
            told(TAKES_PARK_RAISED_FROM_BUILD),
            vec![ENV_LAUNCHER_NOFILE.name(), ENV_PARK_RAISED_FROM.name()],
            "v0.101.0 takes both"
        );
    }
}
