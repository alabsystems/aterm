// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Thread SCHEDULING ROLE, declared once per worker.
//!
//! macOS schedules by Quality of Service, and on Apple Silicon QoS also steers
//! P-core versus E-core placement. A thread that never declares one inherits
//! `QOS_CLASS_DEFAULT`, which sits just below `USER_INITIATED` and therefore
//! competes with the UI thread's work whenever the machine is saturated.
//!
//! That default is wrong for most of this app. aterm spawns dozens of workers —
//! font warmers, image/video encoders, update checkers, package probes, log
//! flushers, the smart-title LLM — and essentially all of them do work no human
//! is waiting on. Left at the default they are indistinguishable, to the
//! scheduler, from the thread drawing the next frame. Under load that is
//! precisely backwards: the cosmetic work keeps its share while keystroke
//! handling waits, which is felt as typing lag rather than as a slow font warm.
//!
//! Declaring the role makes the priority ORDER explicit and reviewable, instead
//! of an accident of which threads happened to get a `pthread_set_qos_class`
//! call. The scale is deliberately coarse — four roles, chosen by asking "who is
//! waiting for this?" — because a finer one invites per-thread tuning that no
//! one can reason about globally.
//!
//! Every function is a no-op off macOS, so call sites stay platform-neutral.
//!
//! PROVENANCE. This module is a HAND-PORT of commit `61a6c8b62`
//! (`fix/event-loop-wake-spin-v2`, 2026-08-11), which found thread QoS to be
//! the second cause of the measured keystroke-dequeue lag: 41 named threads,
//! 2 declaring a class. It is a port and not a merge because main moved ~2100
//! commits past that branch's base before it could land, so no hunk of it
//! applies textually. The enum, its class mapping and `set_self` are carried
//! over verbatim; the docs gain the port-time floor rule below and the tests
//! gain one extra rank assertion; each worker's role is re-derived against
//! today's spawn site. One rule was added at port time
//! that the branch only applied to `aterm-scrollback-compress`: a worker that
//! initialises or holds a lock the UI thread contends (a `Mutex`, or a
//! `OnceLock` the UI thread's own first use would block on) is NOT demoted
//! below [`Role::Responsive`], because a descheduled lock holder at a lower
//! class is a priority inversion — the one hazard a QoS port can introduce.

/// What a thread's work is worth relative to the human at the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// The human is watching this land RIGHT NOW: input egress, and anything
    /// else on the keystroke→glass path. Ranks at or above the UI thread.
    Interactive,
    /// The human asked for it and is waiting, but not frame-by-frame: PTY drain
    /// and parse, reflow. Below the UI thread, above everything cosmetic.
    ///
    /// Also the FLOOR for any worker that holds or initialises a lock the UI
    /// thread contends. Matching the PTY drain here does not make such work
    /// urgent; it shortens the window in which a lock holder can sit
    /// descheduled with the UI thread queued behind it.
    Responsive,
    /// Useful work nobody is blocked on: font warming, encoding, log flushing,
    /// smart titles, package/update probes. Must never delay a keystroke.
    Background,
    /// Work with NO deadline at all, which may therefore run arbitrarily late:
    /// opportunistic disk sweeps and prefetch. Yields to literally everything.
    ///
    /// "Arbitrarily late" is the whole contract, and it is stricter than it
    /// sounds. A `QOS_CLASS_BACKGROUND` thread on a saturated machine can be
    /// starved for many seconds, so anything holding a lock, enforcing a
    /// timeout, or owing cleanup that something else waits on does NOT belong
    /// here. Process REAPERS in particular look like housekeeping and are not:
    /// they enforce a kill-and-reap contract on a real deadline, and a starved
    /// reaper leaks the very processes whose CPU use degrades the machine. They
    /// take [`Role::Background`]. (Pinned by experience — putting the managed
    /// runtime's reapers here made their 500 ms kill-and-reap assertions fail
    /// under load.)
    Housekeeping,
}

#[cfg(target_os = "macos")]
const fn qos_class(role: Role) -> libc::qos_class_t {
    match role {
        Role::Interactive => libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE,
        Role::Responsive => libc::qos_class_t::QOS_CLASS_USER_INITIATED,
        Role::Background => libc::qos_class_t::QOS_CLASS_UTILITY,
        Role::Housekeeping => libc::qos_class_t::QOS_CLASS_BACKGROUND,
    }
}

/// Declare the CALLING thread's role. Call it as the first statement of a
/// worker's closure, so the whole body runs at the declared class.
///
/// Setting a thread's own QoS is the supported spelling (`_self_np`); it cannot
/// fail in a way worth branching on, so the result is discarded.
#[inline]
pub(crate) fn set_self(role: Role) {
    #[cfg(target_os = "macos")]
    // SAFETY: sets this thread's own QoS class. Takes no pointers and mutates
    // no shared state, so there is nothing for another thread to observe.
    unsafe {
        libc::pthread_set_qos_class_self_np(qos_class(role), 0);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = role;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ORDER is the contract: cosmetic work must never outrank the work a
    /// human is waiting on. Pinned as a test because the roles are assigned at
    /// ~20 scattered spawn sites, and a wrong one is invisible at the call site
    /// — it shows up only as latency on a loaded machine.
    #[cfg(target_os = "macos")]
    #[test]
    fn roles_rank_interactive_above_cosmetic_work() {
        let rank = |role| qos_class(role) as u32;
        assert!(
            rank(Role::Interactive) > rank(Role::Responsive),
            "input egress must outrank the PTY drain"
        );
        assert!(
            rank(Role::Responsive) > rank(Role::Background),
            "the PTY drain must outrank font warming and smart titles"
        );
        assert!(
            rank(Role::Background) > rank(Role::Housekeeping),
            "useful background work must outrank the artifact-quarantine sweep"
        );
    }

    /// The inherited class a silent `spawn` leaves a thread at sits BETWEEN the
    /// two roles the floor rule chooses between: demoting a lock holder to
    /// `Background` really is a demotion below what it had, and `Responsive`
    /// really is a promotion. If Apple ever renumbers the classes this is the
    /// assertion that notices.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_undeclared_default_sits_between_responsive_and_background() {
        let default = libc::qos_class_t::QOS_CLASS_DEFAULT as u32;
        assert!(qos_class(Role::Responsive) as u32 > default);
        assert!(default > qos_class(Role::Background) as u32);
    }

    /// Off macOS the call must still compile and do nothing, so call sites never
    /// need a `cfg`.
    #[test]
    fn declaring_a_role_is_infallible_everywhere() {
        set_self(Role::Background);
        set_self(Role::Housekeeping);
    }
}
