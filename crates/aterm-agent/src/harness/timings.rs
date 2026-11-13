// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HARNESS'S CLOCK.

use std::time::Duration;

/// The fast-timers development seam: set in a debug build, the harness runs [`Timings::FAST`].
pub const FAST_SEAM: &str = aterm_types::domain::ENV_DEV_FAST_TIMERS;

/// The harness's bounded waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timings {
    /// A signalled agent's exit (`upgrade_wake::wait_exit`, the kernel's exit event).
    pub exited: Duration,
    /// The shell the agent left taking its terminal back, before the step says `wait:shell-prompt`.
    pub prompt_back: Duration,
    /// The pause after the shell took its terminal back, for it to draw its prompt before the
    /// relaunch line is typed.
    pub prompt_drawn: Duration,
    /// The pause between two fenced tries of a typed line whose screen moved under its fence.
    pub fence_every: Duration,
    /// One step's wait for a relaunched agent to register its conversation.
    pub held: Duration,
    /// [`Self::held`] for a cold restore's relaunch, which releases the one sweep lock for other
    /// tabs sooner.
    pub restored_held: Duration,
    /// How long the shell must hold its terminal, the relaunch line seen to run, before the
    /// relaunched agent reads as ended as it started.
    pub ended_at_once: Duration,
    /// How often the shell-holds-its-terminal look runs (a `ps` each time).
    pub shell_look: Duration,
    /// The relaunched agent's first rest (an idle, a question, a box or a wall), under the
    /// harness's hand.
    pub first_idle: Duration,
    /// A relaunched codex-alab's first idle, under the harness's hand (`codex_alab_drive`'s hold).
    pub alab_first_idle: Duration,
    /// A relaunched agent settling before it is told to carry on.
    pub carry_settle: Duration,
    /// The Muse Code lane: the relaunched TUI found as the shell's child.
    pub muse_found: Duration,
    /// The Muse Code lane: the relaunched TUI's composer drawn.
    pub muse_up: Duration,
    /// The Muse Code lane: the relaunched TUI holding its session's lock.
    pub muse_lock: Duration,
    /// The Muse Code lane: an adopted TUI seen in its tab.
    pub muse_adopt: Duration,
    /// The agy lane: the relaunched TUI found as the shell's child.
    pub agy_found: Duration,
    /// The agy lane: the relaunched TUI's composer drawn.
    pub agy_up: Duration,
    /// The agy lane: the relaunched TUI holding its conversation's presence lock.
    pub agy_lock: Duration,
    /// The agy lane: an adopted TUI seen in its tab.
    pub agy_adopt: Duration,
    /// The supervisor loop: delay before re-deciding a point whose fenced write was skipped.
    pub redecide_after_skip: Duration,
    /// The supervisor loop: the lease TTL its claim on a session asks for.
    pub claim_ttl: Duration,
    /// One exchange on a drive connection to a control socket, with the wait a request names on
    /// top (`RelayClient::bounded`): a hang guard, never a wait a test sits through, so the tests'
    /// clock keeps the product's and a test of a server that never answers names its own.
    pub drive_reply: Duration,
    /// Recording an owner's upgrade word behind a sweep writer. Zero still attempts the
    /// native lock once, but does not wait after contention.
    pub owner_word_wait: Duration,
}

impl Timings {
    /// The product's waits, exactly the constants they replaced.
    pub const PRODUCTION: Self = Self {
        exited: Duration::from_secs(30),
        prompt_back: Duration::from_secs(15),
        prompt_drawn: Duration::from_millis(600),
        fence_every: Duration::from_millis(400),
        held: Duration::from_secs(90),
        restored_held: Duration::from_secs(7),
        ended_at_once: Duration::from_secs(5),
        shell_look: Duration::from_secs(1),
        first_idle: Duration::from_secs(15),
        alab_first_idle: Duration::from_secs(60),
        carry_settle: Duration::from_secs(120),
        muse_found: Duration::from_secs(90),
        muse_up: Duration::from_secs(60),
        muse_lock: Duration::from_secs(30),
        muse_adopt: Duration::from_secs(30),
        agy_found: Duration::from_secs(90),
        agy_up: Duration::from_secs(60),
        agy_lock: Duration::from_secs(30),
        agy_adopt: Duration::from_secs(30),
        redecide_after_skip: Duration::from_secs(2),
        claim_ttl: Duration::from_secs(90),
        drive_reply: crate::DEFAULT_REPLY_WAIT,
        owner_word_wait: Duration::from_secs(60),
    };

    /// The tests' waits.
    pub const FAST: Self = Self {
        exited: Duration::from_secs(10),
        prompt_back: Duration::from_millis(300),
        prompt_drawn: Duration::from_millis(20),
        fence_every: Duration::from_millis(20),
        held: Duration::from_secs(3),
        restored_held: Duration::from_millis(700),
        ended_at_once: Duration::from_millis(300),
        shell_look: Duration::from_millis(50),
        first_idle: Duration::from_millis(1_000),
        alab_first_idle: Duration::from_secs(3),
        carry_settle: Duration::from_secs(3),
        muse_found: Duration::from_secs(3),
        muse_up: Duration::from_secs(2),
        muse_lock: Duration::from_millis(1_000),
        muse_adopt: Duration::from_secs(2),
        agy_found: Duration::from_secs(3),
        agy_up: Duration::from_secs(2),
        agy_lock: Duration::from_millis(1_000),
        agy_adopt: Duration::from_secs(2),
        redecide_after_skip: Duration::from_millis(100),
        claim_ttl: Duration::from_secs(600),
        drive_reply: crate::DEFAULT_REPLY_WAIT,
        owner_word_wait: Duration::from_secs(60),
    };

    /// This build's clock.
    #[must_use]
    pub fn for_build() -> Self {
        if aterm_types::dev_seam!(FAST_SEAM).is_some() {
            Self::FAST
        } else {
            Self::PRODUCTION
        }
    }
}

impl Default for Timings {
    /// [`Timings::for_build`].
    fn default() -> Self {
        Self::for_build()
    }
}

#[cfg(test)]
#[path = "timings_tests.rs"]
mod tests;
