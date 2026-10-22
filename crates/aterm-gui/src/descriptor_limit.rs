// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A process's soft `RLIMIT_NOFILE` across a self-update: the successor's room
//! for the sessions it adopts, the outgoing process's room for its park, and the
//! launcher's limit every shell is handed back. POSIX alone (`getrlimit`,
//! `setrlimit`, `fcntl`), so every Unix lane takes it: macOS's launched lane
//! and both platforms' fork lane and cold lane. Moved out of
//! `handoff_rendezvous`, which is macOS's.

use crate::handoff_env::HandoffName;

/// The soft `RLIMIT_NOFILE` a launched successor raises itself to before it
/// dials (never above its hard limit).
///
/// A LaunchServices-launched app runs at launchd's soft limit, 256 — not the
/// parent's, which a forked successor inherits. A grant of up to
/// [`MAX_CHUNKED_SESSIONS`] masters, and then a wake pipe and an exit
/// witness's `kqueue` (round seven, finding 26) per adopted session, needs
/// about four descriptors a session: at 256 the later chunks'
/// descriptors could not be received at all (the kernel discards rights past
/// the limit and the receive fails `EMSGSIZE`), and at a few dozen sessions
/// fewer every later open failed `EMFILE` (round six of the update audit,
/// item 39). 4096 covers the largest grant with room to spare, and stays under
/// Darwin's `OPEN_MAX` (10240), past which `setrlimit` refuses.
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

/// THE ONE PLACE A SUCCESSOR SETTLES ITS SOFT DESCRIPTOR LIMIT, whichever lane
/// its sessions arrive on — `main_entry` runs it once, after the boot apply and
/// before any thread, the launched lane's dial or the fork lane's intake.
///
/// First the launcher's limit a predecessor carried is adopted
/// ([`adopt_carried_launcher_limit`]), so new tabs get it. Then, when this
/// launch carries a handoff — a rendezvous to dial, or the descriptor list a
/// fork-lane parent set — the limit is raised to
/// [`SUCCESSOR_DESCRIPTOR_FLOOR`] ([`raised_soft_limit`]): room for the grant,
/// and for the wake pipe and exit witness each adopted session opens.
///
/// THE FORK LANE NEEDS IT TOO. Only the launched lane's
/// claim raised the limit, and a forked successor inherits its predecessor's. A
/// producer that never raised its own — any build before round eight's park
/// raise, started from the Dock at launchd's 256 — handed its sessions to a
/// successor that then added three descriptors for each, and from a few dozen
/// sessions ran out of them after Commit. A cold launch raises nothing.
///
/// What the raise leaves on record: a launched successor's is a claim's, as it
/// always was ([`PARK_RAISED_FROM`] is cleared); a forked successor's own raise
/// is one no successor made before it, recorded as a park's is, so an older
/// successor of this process is started at the limit held before it
/// ([`OlderImageLimit`]). A forked successor that inherited room raises
/// nothing, and keeps whatever record it adopted.
///
/// Every Unix successor settles here: the fork lane is
/// POSIX, and a Linux fork-lane successor once kept whatever limit its producer
/// held, with no raise and no carried launcher limit. Only macOS has the
/// launched lane's rendezvous; elsewhere nothing sets it.
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

/// The soft limit to raise to from `soft` under `hard`, or `None` when it is
/// already at least [`SUCCESSOR_DESCRIPTOR_FLOOR`] (or the hard limit).
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

/// The soft descriptor limit a shell this process spawns must be handed back
/// (round six of the update audit, item 39, review round two): `Some(soft)`
/// when a claim, a park or a recovery raised this process's own limit from
/// `soft` (or a predecessor carried it), `None` when none did and the
/// process's limit is still the launcher's.
///
/// The raise is the SUCCESSOR's need — room for the grant and the sessions it
/// adopts — never its shells'. A terminal hands its shell the launcher's soft
/// limits in both directions (`aterm_cli`'s `session_limits` records why), so a
/// tab opened after an update through the launched lane must see launchd's 256,
/// exactly as a Dock-launched aterm's does, and not the 4096 this process
/// raised itself to.
pub(crate) fn launcher_soft_descriptor_limit() -> Option<u64> {
    LAUNCHER_SOFT_NOFILE.get().copied()
}

/// The soft `RLIMIT_NOFILE` this process would hold had no PARK raised it
/// (round eight's second review of #3); 0 when it holds no park's raise.
///
/// Only [`raise_descriptor_limit_for_park`] is new in round eight: a claim's
/// raise (round six, item 39) and a recovery's (round seven, finding 26)
/// were made before it, so a successor of a process they raised always
/// inherited the raise. A park's raise is the one nothing before it made, and
/// so the one an OLDER successor must not be started at
/// ([`OlderImageLimit`]): set by the park's raise that moved the limit, by a
/// forked successor's own raise (U9 — none was made before
/// it either), or adopted from a predecessor that held one
/// ([`ENV_PARK_RAISED_FROM`]), and cleared by a claim's or a recovery's, after
/// which the raise is one the process would have held anyway.
static PARK_RAISED_FROM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// See [`PARK_RAISED_FROM`]: `Some(soft)` while this process holds a raise
/// only a park made.
pub(crate) fn park_raised_from() -> Option<u64> {
    Some(PARK_RAISED_FROM.load(std::sync::atomic::Ordering::Relaxed)).filter(|from| *from > 0)
}

/// The raise this process now holds is one a claim or a recovery makes
/// whatever a park did before it (see [`PARK_RAISED_FROM`]).
fn forget_park_raise() {
    PARK_RAISED_FROM.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// The launch variable in which a predecessor tells its successor the soft
/// descriptor limit it held before a PARK raised it (round eight's second
/// review of #3), beside [`ENV_LAUNCHER_NOFILE`] and for the same two lanes
/// ([`carry_launcher_limit`]): a successor that inherits a park's raise is
/// told so, and hands an older successor of its own what it would have held
/// without it. Read by [`adopt_carried_launcher_limit`]; a successor that
/// predates it ignores it.
pub(crate) const ENV_PARK_RAISED_FROM: HandoffName =
    HandoffName::new("ATERM_HANDOFF_PARK_RAISED_FROM");

/// The launch variable in which a FORK-LANE parent tells its successor the
/// soft descriptor limit its shells are owed (round seven, item 107): the
/// launcher's, when [`launcher_soft_descriptor_limit`] knows it. A forked
/// successor inherits the parent's RAISED soft limit and never claims, so
/// without this every tab it opened got 4096 back — the launcher's 256 held
/// for one hop only. Read by [`adopt_carried_launcher_limit`].
pub(crate) const ENV_LAUNCHER_NOFILE: HandoffName =
    HandoffName::new("ATERM_HANDOFF_LAUNCHER_NOFILE");

/// v0.100.0's build (`RELEASES.ledger`; no v0.99.0 was cut): the first
/// release that takes [`ENV_LAUNCHER_NOFILE`] out of its environment at its
/// capture (06b06e74e). A build before it leaves the name in its `environ`, so
/// every shell it opens inherits it.
const TAKES_LAUNCHER_NOFILE_FROM_BUILD: u64 = 1_790_807_622;

/// v0.101.0's build: the first release that takes [`ENV_PARK_RAISED_FROM`]
/// out of its environment (ddaf83ae2f, after v0.100.0 was cut).
const TAKES_PARK_RAISED_FROM_BUILD: u64 = 1_791_501_389;

/// Name [`ENV_LAUNCHER_NOFILE`] on a successor's `command` whose image keeps
/// this process's soft descriptor limit — the fork lane's child and the cold
/// lane's `exec` alike. Nothing when this process never raised its limit, so
/// its shells already get what it runs at.
///
/// ONLY TO A TARGET THAT TAKES IT. A successor built
/// before the name ([`TAKES_LAUNCHER_NOFILE_FROM_BUILD`]) never reads it and
/// leaves it in its environment: every shell of a rollback to it carried the
/// launcher's limit, and a newer aterm started from one adopted it as its own
/// and handed every tab of its window that limit, whatever the shell ran at.
/// Such a successor's shells get what it inherits, as before 06b06e74e; one
/// built before [`ENV_PARK_RAISED_FROM`] is told the launcher's limit alone.
///
/// THE COLD LANE OWES IT TOO (round eight of the update audit, #81): `execve`
/// keeps the raised soft limit exactly as a fork does, and the new image's
/// record starts empty, so a cold-lane update after a launched-lane one (or
/// after any attempt that raised the limit for its park) handed every later
/// tab 4096 instead of the launcher's 256 — the leak item 107 closed for the
/// fork-lane hop only.
///
/// A PARK'S RAISE IS NAMED BESIDE IT (round eight's second review of #3,
/// [`ENV_PARK_RAISED_FROM`]): the successor keeps that raise too, and must
/// know it is one no successor inherited before round eight.
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

/// A CARRIED LIMIT IS AN UPDATE'S WORD TO ITS SUCCESSOR, AND NOTHING ELSE'S
/// . Every lane that names [`ENV_LAUNCHER_NOFILE`] or
/// [`ENV_PARK_RAISED_FROM`] — the fork lane's child, the cold lane's `exec` —
/// names `ATERM_UPDATED_FROM` beside it (`update_launch`), and every build
/// takes that one out of its environment before any shell. A copy on any
/// other launch is a stray: a successor built before the name left it in its
/// shells' environment (every release up to v0.99.0, handed it by a later
/// release's rollback), and a window started from one of them adopted it and
/// handed every tab that limit — 256 where the shell ran at 4096, so builds
/// and servers there failed `EMFILE`.
///
/// Dropped from the snapshot here, right after the capture and before the
/// boot apply, whose re-exec names `ATERM_UPDATED_FROM` itself and carries
/// the snapshot on: dropped later, the stray would reach that image as an
/// update's word.
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

/// Record the launcher limit a fork-lane parent carried
/// ([`ENV_LAUNCHER_NOFILE`]) as this process's own
/// [`launcher_soft_descriptor_limit`], before any shell is spawned. Read, not
/// consumed: a boot re-exec carries the snapshot on, and the final image reads
/// it again. `Some(limit)` when it was adopted. A park's raise carried beside
/// it ([`ENV_PARK_RAISED_FROM`]) is adopted as [`park_raised_from`] here too.
pub(crate) fn adopt_carried_launcher_limit(env: &crate::handoff_env::HandoffEnv) -> Option<u64> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a valid resource id and an exclusively borrowed, writable
    // out-parameter for the one call.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
        return None;
    }
    // A park's raise the predecessor held, and this process inherited with
    // its limit (round eight's second review of #3): the same bounds, since it
    // too only ever names less than this process runs at.
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

/// Raise this process's soft `RLIMIT_NOFILE` to [`raised_soft_limit`]'s
/// answer: `Ok(Some((from, to)))` when it moved, `Ok(None)` when it had room
/// already. Raising a soft limit up to the hard one needs no privilege. The
/// limit it moved FROM is recorded for [`launcher_soft_descriptor_limit`].
pub(crate) fn raise_descriptor_limit_for_claim() -> std::io::Result<Option<(u64, u64)>> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a valid resource id and an exclusively borrowed, writable
    // out-parameter for the one call.
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
    // SAFETY: a valid resource id and a fully initialised rlimit that keeps
    // the hard limit as it was.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const raised) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // The first raise saw the launcher's value; a later one cannot happen (the
    // limit is at the floor or the hard limit now), and would not be it.
    let _ = LAUNCHER_SOFT_NOFILE.set(soft);
    Ok(Some((soft, target)))
}

/// THE KEEPER'S RECOVERY needs the same room (round seven of the update
/// audit, finding 26's review): a cold launch with `[keeper] enabled` takes
/// every master a crashed window left in the keeper's custody at its boot
/// HELLO and adopts each — a master, a wake pipe and an exit witness's
/// `kqueue` apiece — at launchd's soft 256, where only a handoff's claim
/// raised it before. Raised before the HELLO, as the claim raises before it
/// dials; a shell spawned afterwards is still handed the launcher's limit
/// ([`launcher_soft_descriptor_limit`]). A failure is said, and the recovery
/// goes ahead as it would have.
pub(crate) fn raise_descriptor_limit_for_recovery() {
    // Either way the limit held now is one the recovery makes whatever a
    // park did before it ([`PARK_RAISED_FROM`]).
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

/// THE OUTGOING PROCESS needs the same room at its park (round eight of the
/// update audit, #3). A window started from the Dock, Finder or a login item
/// runs at launchd's soft 256, and each of its sessions already holds three
/// descriptors (the master and both ends of the reader's wake pipe); both
/// lanes then duplicate every master before the park, and the attempt adds
/// its pipes, the rendezvous and the accepted dial. From about 60 sessions
/// that no longer fits — every attempt failed `EMFILE` at the duplicate and
/// was retried on the transient schedule for the same fixed cause, and the
/// desks the chunked grant exists for could never park. Raised when an
/// attempt takes the seamless lane, before either lane reserves anything; a
/// forked successor inherits the room and is told the launcher's limit
/// ([`carry_launcher_limit`]), and a shell spawned afterwards is still
/// handed the launcher's ([`launcher_soft_descriptor_limit`]). A failure is
/// said, and the attempt goes ahead as it would have.
///
/// NOT TOWARD AN OLDER TARGET, AND NOT INTO ONE (round eight's two reviews
/// of #3). An attempt whose `target_build` is older than this `build` — an
/// installed activation of an older bundle put back in /Applications, a
/// channel switch — makes no raise: a successor built before 06b06e74e (every
/// release up to v0.99.0) never reads [`ENV_LAUNCHER_NOFILE`], and would hand
/// every one of its shells a raise it inherited, where a Dock-launched
/// window's older successor always handed them launchd's 256. But nothing
/// lowers this process's limit again, so the raise an EARLIER attempt made
/// toward a same-or-newer build — one that stood down after it, as automatic
/// attempts often do — is still held when a later one targets an older
/// build. So the raise is recorded as a park's ([`park_raised_from`]), and
/// both lanes that can start an older successor start it at the limit this
/// process held before the raise ([`OlderImageLimit`]). A successor this
/// build knows reads the told limit, so the raise costs its shells nothing.
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

/// THE LIMIT AN OLDER SUCCESSOR IS STARTED AT (round eight's second review
/// of #3). A park's raise ([`raise_descriptor_limit_for_park`]) is made once
/// and kept, and a successor inherits it across a fork and across an `exec`
/// alike; one built before 06b06e74e never reads the launcher's limit it is
/// told, so every shell it opened got the raise — where, before round eight,
/// a Dock-launched window's older successor inherited the launcher's limit
/// and handed its shells that. So toward a target OLDER than this build, both
/// lanes that can reach one (the fork lane, the only seamless lane toward it,
/// and the cold lane's `exec`) start its image at the limit this process held
/// before the park's raise ([`park_raised_from`]). A limit a claim or a
/// recovery raised is left as it always was: a successor inherited that one
/// before round eight too.
///
/// ONLY WHERE THE IMAGE FITS UNDER IT. An older image's shell strip reaches
/// only the numbers below the soft limit (its `inherited_fd_ceiling`; this
/// build's strip reaches the kernel's maximum instead), so no descriptor the
/// image keeps across its `exec` may sit at or above the limit it is started
/// at. A desk that fits
/// under the launcher's limit normally hands it nothing up there (a
/// duplicate takes the lowest free number). One that needed the park's raise
/// to duplicate its masters — a desk an older target could not be parked
/// with before round eight, when the duplicate failed `EMFILE` — hands the
/// successor duplicates above it, and that successor keeps the raise it
/// inherits, as the log says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OlderImageLimit {
    /// The soft limit the image is started at: this process's before the
    /// park's raise.
    soft: u64,
    /// The limits this process runs at, the hard one kept as it is.
    raised: (u64, u64),
    /// One past the highest descriptor number this process can hold
    /// ([`aterm_uds::spawnfd::descriptor_bound`]), read with the limits.
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
        // SAFETY: a valid resource id and an exclusively borrowed, writable
        // out-parameter for the one call.
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
    /// open, and not close-on-exec. The scan ends at the kernel's per-process
    /// maximum, not at the soft limit or a cap: Darwin hands out numbers up to
    /// `kern.maxfilesperproc` whenever the soft limit is higher, and the old
    /// 65 536 cap missed every one above it. `fcntl` and arithmetic only —
    /// async-signal-safe, so a forked child may ask between its fork and its
    /// `exec`.
    fn inheritable_at_or_above(&self) -> Option<libc::c_int> {
        let floor = libc::c_int::try_from(self.soft).unwrap_or(libc::c_int::MAX);
        (floor..self.table).find(|fd| {
            // SAFETY: `F_GETFD` only reads one number's flags; a number that
            // is not open answers -1.
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
        // SAFETY: a valid resource id and a fully initialised rlimit that
        // keeps the hard limit as it was.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Start the FORK LANE's successor `command` at [`OlderImageLimit`] when its
/// `target_build` is older than this `build`. Register it after every other
/// pre-exec hook of the command: the lane's own clears close-on-exec on the
/// descriptors the child is handed (`inherited`), and the child's last check
/// must see them as its image will.
///
/// A descriptor the child would keep at or above the limit — one in
/// `inherited`, or one of this process's own that is not close-on-exec —
/// leaves the child the raise it inherits, and says so here; the child looks
/// at its own table again just before its `exec`, and lowers nothing over a
/// descriptor that appeared since.
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
    // SAFETY: the closure runs between fork and exec and calls only `fcntl`
    // and `setrlimit` over integers it owns — no allocation, no lock.
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

/// [`OlderImageLimit`] for the COLD LANE's `exec`, which replaces THIS
/// process: lowered here, immediately before the `exec`, and raised back when
/// the guard drops (the `exec` returned). Only when no descriptor the `exec`
/// keeps sits at or above the new limit, so the older image, whose strip
/// reaches only the numbers below its soft limit, is handed none above it
/// (this build's strips no longer read the soft limit at all); the cold lane
/// runs with no session open, so its table sits far below launchd's 256. Said
/// either way.
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

/// See [`lower_for_older_image`]: dropped only when the `exec` returned.
pub(crate) struct LoweredForOlderImage {
    limit: OlderImageLimit,
}

impl Drop for LoweredForOlderImage {
    fn drop(&mut self) {
        let raised = self.limit.raised.0;
        // Raising a soft limit back up to where it was, under the hard one,
        // needs no privilege.
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
        // SAFETY: fresh pipe descriptors, exclusively owned from here.
        unsafe { (OwnedFd::from_raw_fd(raw[0]), OwnedFd::from_raw_fd(raw[1])) }
    }

    /// THE LAUNCHER'S LIMIT OUTLIVES A FORK-LANE HOP (round seven, item 107).
    /// A launched successor records the soft limit it raised from; its next
    /// update may take the fork lane, whose child inherits the raised limit and
    /// never claims. The parent names the recorded limit on the child's
    /// command, and the child adopts it — only ever lower than what it runs at,
    /// and only a number.
    ///
    /// RED before the fix: nothing carried it, so a forked successor's shells
    /// got the raised 4096 back.
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

    /// THE LAUNCHER'S LIMIT OUTLIVES A COLD-LANE EXEC TOO (round eight of the
    /// update audit, #81). A process that raised its soft descriptor limit — a
    /// launched successor's claim, or any attempt's park — later updates with
    /// no terminal open, so the cold lane `exec`s. `execve` keeps the raised
    /// limit, and the new image's record starts empty: unless the exec names
    /// the launcher's limit, every tab the new image opens gets the raise.
    ///
    /// End to end, in children of the test binary: a raised process's command
    /// carries it into a new image that runs at the raised limit, adopts the
    /// carried one through the production intake (`HandoffEnv::capture`, then
    /// `adopt_carried_launcher_limit`), and hands a shell the launcher's.
    ///
    /// RED before the fix: the cold lane's `Command` named only its argv and
    /// `ATERM_UPDATED_FROM`, and the new image's shells read 4096.
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
            // SAFETY: a valid resource id and a writable out-parameter.
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
        // The new image is this test again, as a stand-in: it passes only by
        // running exactly this test (its shells got the launcher's limit).
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
        // SAFETY: a valid resource id and a writable out-parameter.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) },
            0
        );
        limit.rlim_cur
    }

    /// Mark every descriptor at or above `floor` close-on-exec: what a busy
    /// test run leaked into a child of the test binary there is not the
    /// desk's, and a window at that limit could hold none. Up to the kernel's
    /// per-process maximum, not this process's limit: a child of the test
    /// binary inherits the leak at the numbers it had in the parent, above the
    /// limit the child was then given.
    fn mark_close_on_exec_at_or_above(floor: u64) {
        let floor = i32::try_from(floor).expect("a descriptor number");
        for fd in floor..aterm_uds::spawnfd::descriptor_bound() {
            // SAFETY: F_GETFD/F_SETFD on any number only reads or sets its
            // close-on-exec flag; a number that is not open answers -1.
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
        // std's Command on purpose: the shipped `start_older_image_at_its_limit` hook takes std's and adds a `pre_exec`.
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

    /// AN OLDER SUCCESSOR STARTS AT THE LIMIT HELD BEFORE A PARK'S RAISE
    /// (round eight's second review of #3). A Dock-launched window, at
    /// launchd's soft limit, begins an attempt toward this build (or a newer
    /// one) that raises its limit for the park and then stands down — the
    /// activity re-check, a recording, a launched-lane hold. Nothing lowers
    /// that raise again. Later the person puts an older bundle back, or
    /// switches channel, and an attempt targets an OLDER build: no raise is
    /// made for it, but the one already made is still held — and a successor
    /// built before 06b06e74e never reads the launcher's limit it is told, so
    /// every shell it opened got the raise, where before round eight it
    /// inherited launchd's limit. Driven in a child of the test binary whose
    /// limit is launchd's in miniature: the fork lane's hook starts the older
    /// successor at the launchd limit (its stand-in's `ulimit -n` reads it),
    /// the cold lane's lowers it for the `exec` and raises it back when the
    /// `exec` returns, and a successor started at the raise is told of it and
    /// does the same for an older successor of its own. NEGATIVE CONTROLS: a
    /// same-or-newer target inherits the room; a descriptor the image would
    /// keep at or above the launchd limit leaves the raise (its strip would
    /// not reach it); and a raise a recovery or a claim made — one every
    /// successor inherited before round eight — is left as it was.
    ///
    /// RED before the fix: the older successor's stand-in read the raise.
    #[test]
    fn an_older_successor_starts_at_the_limit_held_before_a_parks_raise() {
        const NEW_IMAGE: &str = "ATERM_TEST_PARK_RAISE_NEW_IMAGE";
        const BUILD: u64 = 7;

        // THE DECISION: only an older target, only under a park's raise.
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
            // A SUCCESSOR STARTED AT THE RAISE, told of it through the
            // production intake…
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
            // …starts an older successor of its own at the launcher's limit.
            assert_eq!(successor_soft_nofile(BUILD - 1, BUILD, &[]), launcher);
            return;
        }

        if !crate::control_auth::enter_low_nofile_test_child(64) {
            return;
        }
        let launchd = soft_nofile();
        mark_close_on_exec_at_or_above(launchd);
        // CONTROL: nothing raised, nothing lowered.
        assert_eq!(OlderImageLimit::toward(BUILD - 1, BUILD), None);
        assert_eq!(successor_soft_nofile(BUILD - 1, BUILD, &[]), launchd);
        // An attempt toward an older build makes no raise; one toward this
        // build does, and stands down — the raise is kept.
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

        // THE REVIEW'S SCENARIO: a later attempt toward an older build.
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

        // A DESCRIPTOR THE IMAGE WOULD KEEP AT OR ABOVE launchd's limit — one
        // it is handed, or a stray of this process's — leaves the raise: its
        // shells' strip would not reach it.
        let (read, _write) = pipe_for_test();
        let launchd_fd = i32::try_from(launchd).expect("a descriptor number");
        // SAFETY: F_DUPFD duplicates a live descriptor at the first free
        // number at or above `launchd_fd`, with close-on-exec clear.
        let high = unsafe { libc::fcntl(read.as_raw_fd(), libc::F_DUPFD, launchd_fd) };
        assert!(
            high >= launchd_fd,
            "PRECONDITION: room above launchd's limit"
        );
        // SAFETY: a fresh duplicate, owned here from now on.
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

        // THE COLD LANE: lowered for the exec, raised back if it returns.
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

        // ACROSS A HOP: a successor started at the raise is told it is a
        // park's, beside the launcher's limit, on both lanes' commands.
        // The successor is this test again, as a stand-in: it passes only by
        // running exactly this test (it started its older successor at
        // launchd's limit).
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

        // A RECOVERY'S RAISE (a claim's alike) is one a successor inherited
        // before round eight: after it, an older successor inherits the
        // raise, as it always did.
        raise_descriptor_limit_for_recovery();
        assert_eq!(park_raised_from(), None, "the raise is a recovery's now");
        assert_eq!(successor_soft_nofile(BUILD - 1, BUILD, &[]), raised);
    }

    /// …AND THE RAISE STAYS THE SUCCESSOR'S (round six, item 39, review round
    /// two). A shell this process spawns after the claim — a new tab, with the
    /// daily-driver limits `spawn_session` hands the PTY — must see the soft
    /// limit the process was launched with, not the one it raised itself to.
    /// Driven through the real actuator (`Limits::apply` before `exec`) and read
    /// back by the shell's own `ulimit -n`.
    ///
    /// RED before the fix: the daily-driver modes passed `Limits::inherit()`,
    /// so the shell read the raised limit.
    pub(crate) fn a_shell_spawned_after_the_raise_gets_the_launchers_limit(from: u64, to: u64) {
        use std::os::unix::process::CommandExt as _;
        assert_eq!(
            launcher_soft_descriptor_limit(),
            Some(from),
            "the raise was recorded"
        );
        let limits = crate::spawn::daily_driver_limits();
        assert_eq!(limits.open_files, Some(from));
        // SAFETY: test-only root authority, used for this one child's cap.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        // std's Command on purpose: the child applies `Limits` in a `pre_exec`, as the PTY's child does.
        let mut shell = std::process::Command::new("/bin/sh");
        shell.args(["-c", "ulimit -n"]);
        // SAFETY: `apply` is a bare `getrlimit`/`setrlimit` loop with a valid
        // cap — async-signal-safe, as the PTY's own post-fork child relies on.
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
    /// U9). In a child whose soft limit leaves 64 to spare: a cold launch
    /// raises nothing; a launched successor raises and its raise is a claim's
    /// (a park's record is cleared); a second settle with room raises nothing
    /// and leaves the record alone. The fork lane's raise and its intake are
    /// driven end to end by
    /// `a_fork_lane_successor_makes_room_for_the_sessions_it_adopts`.
    #[test]
    fn every_lane_settles_its_descriptor_limit_in_one_place() {
        fn soft_limit() -> u64 {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: a valid resource id and a writable out-parameter.
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
    /// out as every build does. A current window started from one of those
    /// shells used to adopt the launcher's limit as its own and hand every tab
    /// that limit, whatever the shell ran at. Driven through the steps
    /// `main_entry` runs, in its order: the capture, the stray filter, the
    /// settle — and the boot apply's re-exec, which names `ATERM_UPDATED_FROM`
    /// itself and carries the snapshot on, must not carry the stray to the next
    /// image as an update's word. CONTROL: the update's own successor, named
    /// both beside `ATERM_UPDATED_FROM`, adopts them.
    ///
    /// RED before the fix: the window adopted the stray launcher's limit.
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
                // A window started from one of its shells: the names
                // inherited, `ATERM_UPDATED_FROM` taken out.
                let mut window = crate::test_alone::StandIn::this_test();
                window
                    .command()
                    .env(ROLE, "window")
                    .env_remove("ATERM_UPDATED_FROM");
                window.spawn().passed();
                // CONTROL: launched by the update, they are its word.
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
                // Its boot apply's re-exec: the snapshot carried on, and
                // `ATERM_UPDATED_FROM` named by the re-exec.
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
    /// Toward v0.98.0 neither is named, toward v0.100.0 the launcher's limit
    /// alone, and from v0.101.0 on both.
    ///
    /// RED before the fix: both lanes named both toward every target.
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
            // std's Command on purpose: the shipped helper takes std's, and
            // nothing is spawned.
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
