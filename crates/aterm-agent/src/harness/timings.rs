// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HARNESS'S CLOCK.

use std::time::Duration;

/// The fast-timers development seam: set in a debug build, the harness runs [`Timings::FAST`].
pub const FAST_SEAM: &str = aterm_types::domain::ENV_DEV_FAST_TIMERS;

/// The harness's bounded waits (module doc).
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
    /// The relaunched agent's first idle, under the harness's hand.
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
    /// The Gemini CLI lane: the relaunched parent found as the shell's child.
    pub gemini_found: Duration,
    /// The Gemini CLI lane: the relaunched TUI's composer drawn.
    pub gemini_up: Duration,
    /// The Gemini CLI lane: the start-time rule naming the relaunched process's session.
    pub gemini_witness: Duration,
    /// The Gemini CLI lane: an adopted TUI seen in its tab.
    pub gemini_adopt: Duration,
    /// The supervisor loop: delay before re-deciding a point whose fenced write was skipped.
    pub redecide_after_skip: Duration,
    /// The supervisor loop: the lease TTL its claim on a session asks for.
    pub claim_ttl: Duration,
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
        gemini_found: Duration::from_secs(90),
        gemini_up: Duration::from_secs(60),
        gemini_witness: Duration::from_secs(30),
        gemini_adopt: Duration::from_secs(30),
        redecide_after_skip: Duration::from_secs(2),
        claim_ttl: Duration::from_secs(90),
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
        gemini_found: Duration::from_secs(3),
        gemini_up: Duration::from_secs(2),
        gemini_witness: Duration::from_millis(1_000),
        gemini_adopt: Duration::from_secs(2),
        redecide_after_skip: Duration::from_millis(100),
        claim_ttl: Duration::from_secs(600),
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
mod tests {
    use super::{FAST_SEAM, Timings};

    /// The fast clock keeps every ORDER the production one has between its
    /// waits — a cold restore's wait outlasts the ended-at-once count, a look
    /// is shorter than that count — and is shorter or equal everywhere; and
    /// the seam is denied to every child shell, by this very name.
    #[test]
    fn the_fast_clock_keeps_the_production_orders_and_its_seam_is_never_inherited() {
        for t in [Timings::PRODUCTION, Timings::FAST] {
            assert!(t.restored_held > t.ended_at_once, "{t:?}");
            assert!(t.shell_look < t.ended_at_once, "{t:?}");
            assert!(t.restored_held < t.held, "{t:?}");
        }
        let (p, f) = (Timings::PRODUCTION, Timings::FAST);
        assert!(
            f.claim_ttl > aterm_spec::wait::MUST_HAPPEN,
            "a lease never lapses inside the hang detector a release is waited for under"
        );
        for (prod, fast) in [
            (p.exited, f.exited),
            (p.prompt_back, f.prompt_back),
            (p.prompt_drawn, f.prompt_drawn),
            (p.fence_every, f.fence_every),
            (p.held, f.held),
            (p.restored_held, f.restored_held),
            (p.ended_at_once, f.ended_at_once),
            (p.shell_look, f.shell_look),
            (p.first_idle, f.first_idle),
            (p.alab_first_idle, f.alab_first_idle),
            (p.carry_settle, f.carry_settle),
            (p.muse_found, f.muse_found),
            (p.muse_up, f.muse_up),
            (p.muse_lock, f.muse_lock),
            (p.muse_adopt, f.muse_adopt),
            (p.agy_found, f.agy_found),
            (p.agy_up, f.agy_up),
            (p.agy_lock, f.agy_lock),
            (p.agy_adopt, f.agy_adopt),
            (p.gemini_found, f.gemini_found),
            (p.gemini_up, f.gemini_up),
            (p.gemini_witness, f.gemini_witness),
            (p.gemini_adopt, f.gemini_adopt),
            (p.redecide_after_skip, f.redecide_after_skip),
        ] {
            assert!(fast <= prod, "{fast:?} > {prod:?}");
        }
        assert!(aterm_types::domain::is_ai_env_var(FAST_SEAM));
    }
}
