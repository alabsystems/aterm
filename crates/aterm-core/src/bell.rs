// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bell presentation state — the pure decision logic a host needs to turn
//! BEL callbacks into user-visible output (visual flash, audible beep).
//!
//! Hosts own the I/O (painting an inverted frame, playing a sound, marking
//! the window urgent); this module owns the *when*. Every method takes the
//! current [`Instant`] as a parameter instead of reading the clock, so the
//! full transition space is unit-testable without sleeping.

// web_time::Instant is std::time::Instant on native (byte-identical) and a JS-clock
// instant on wasm32, where Instant::now() panics. Every Instant in the engine's clock
// seam (transient.process_now, last_bell_time, ClockReading) must be this same type so
// they interoperate. Duration is unaffected by wasm and stays std::time.
use std::time::Duration;
use web_time::Instant;

/// How long a visual bell keeps the frame flashed (inverted/dimmed).
pub const FLASH_DURATION: Duration = Duration::from_millis(100);

/// Visual-bell flash state machine.
///
/// [`ring`](Self::ring) arms a flash ending [`FLASH_DURATION`] later;
/// [`is_active`](Self::is_active) says whether the current frame should be
/// drawn flashed; [`deadline`](Self::deadline) is the instant the host must
/// wake to repaint the normal frame — `None` while idle, so a host that
/// schedules wakeups only from `deadline` runs zero timers between bells.
#[derive(Debug, Default)]
pub struct BellFlash {
    deadline: Option<Instant>,
}

impl BellFlash {
    /// A new, inactive flash (no deadline, nothing to paint).
    pub fn new() -> Self {
        Self::default()
    }

    /// A bell rang at `now`: (re)start the flash window. Ringing during an
    /// active flash extends it, so a bell flood reads as one continuous
    /// flash rather than strobing.
    pub fn ring(&mut self, now: Instant) {
        self.deadline = Some(now + FLASH_DURATION);
    }

    /// Whether the frame should be drawn flashed at `now`.
    pub fn is_active(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|d| now < d)
    }

    /// The wakeup deadline while a flash is pending, `None` when idle.
    ///
    /// Stays `Some` until [`expire`](Self::expire) observes the deadline
    /// passing, so a host that missed the exact instant still wakes up and
    /// repaints rather than presenting a stuck-inverted frame.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Retire a finished flash: when the deadline has passed, clear it and
    /// return `true` (the host must repaint the normal frame). A still
    /// running or idle flash is left untouched and returns `false`.
    pub fn expire(&mut self, now: Instant) -> bool {
        match self.deadline {
            Some(d) if now >= d => {
                self.deadline = None;
                true
            }
            _ => false,
        }
    }
}

/// Minimum-interval gate for the audible bell: at most one beep per
/// `interval`, however fast BEL arrives. The visual flash still re-arms on
/// every bell, so a flood stays visible without becoming a wall of sound.
#[derive(Debug)]
pub struct BellRateLimiter {
    interval: Duration,
    last: Option<Instant>,
}

impl BellRateLimiter {
    /// A gate that allows at most one firing per `interval`.
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    /// Whether a beep may fire at `now`; records the firing when allowed.
    /// The first call always fires.
    pub fn try_fire(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|l| now.duration_since(l) < self.interval)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flash_starts_idle() {
        let flash = BellFlash::new();
        let now = Instant::now();
        assert!(!flash.is_active(now));
        assert!(flash.deadline().is_none());
    }

    #[test]
    fn ring_activates_for_flash_duration() {
        let mut flash = BellFlash::new();
        let t0 = Instant::now();
        flash.ring(t0);
        assert!(flash.is_active(t0));
        assert!(flash.is_active(t0 + FLASH_DURATION - Duration::from_millis(1)));
        assert!(!flash.is_active(t0 + FLASH_DURATION));
        assert_eq!(flash.deadline(), Some(t0 + FLASH_DURATION));
    }

    #[test]
    fn expire_clears_only_after_deadline() {
        let mut flash = BellFlash::new();
        let t0 = Instant::now();
        flash.ring(t0);
        // Mid-flash: not expired, still active, deadline still armed.
        assert!(!flash.expire(t0 + Duration::from_millis(50)));
        assert!(flash.is_active(t0 + Duration::from_millis(50)));
        // At the deadline: expires exactly once, then idle.
        let end = t0 + FLASH_DURATION;
        assert!(flash.expire(end));
        assert!(flash.deadline().is_none());
        assert!(!flash.is_active(end));
        assert!(!flash.expire(end));
    }

    #[test]
    fn ring_during_flash_extends_deadline() {
        let mut flash = BellFlash::new();
        let t0 = Instant::now();
        flash.ring(t0);
        let t1 = t0 + Duration::from_millis(60);
        flash.ring(t1);
        assert_eq!(flash.deadline(), Some(t1 + FLASH_DURATION));
        // Still active past the original deadline.
        assert!(flash.is_active(t0 + FLASH_DURATION + Duration::from_millis(10)));
    }

    #[test]
    fn deadline_persists_until_expired() {
        // A host that wakes late must still see the deadline (and expire it)
        // rather than find it silently gone.
        let mut flash = BellFlash::new();
        let t0 = Instant::now();
        flash.ring(t0);
        let late = t0 + FLASH_DURATION + Duration::from_secs(5);
        assert_eq!(flash.deadline(), Some(t0 + FLASH_DURATION));
        assert!(!flash.is_active(late));
        assert!(flash.expire(late));
        assert!(flash.deadline().is_none());
    }

    #[test]
    fn rate_limiter_first_fire_always_allowed() {
        let mut gate = BellRateLimiter::new(Duration::from_secs(1));
        assert!(gate.try_fire(Instant::now()));
    }

    #[test]
    fn rate_limiter_blocks_within_interval() {
        let mut gate = BellRateLimiter::new(Duration::from_secs(1));
        let t0 = Instant::now();
        assert!(gate.try_fire(t0));
        assert!(!gate.try_fire(t0));
        assert!(!gate.try_fire(t0 + Duration::from_millis(999)));
    }

    #[test]
    fn rate_limiter_allows_at_interval_boundary() {
        let mut gate = BellRateLimiter::new(Duration::from_secs(1));
        let t0 = Instant::now();
        assert!(gate.try_fire(t0));
        assert!(gate.try_fire(t0 + Duration::from_secs(1)));
        // The boundary fire re-arms the gate from ITS timestamp.
        assert!(!gate.try_fire(t0 + Duration::from_millis(1500)));
        assert!(gate.try_fire(t0 + Duration::from_secs(2)));
    }

    #[test]
    fn rate_limiter_denied_fire_does_not_rearm() {
        // A blocked attempt must not push the window forward (otherwise a
        // steady flood faster than the interval would silence the bell
        // forever).
        let mut gate = BellRateLimiter::new(Duration::from_secs(1));
        let t0 = Instant::now();
        assert!(gate.try_fire(t0));
        assert!(!gate.try_fire(t0 + Duration::from_millis(900)));
        assert!(gate.try_fire(t0 + Duration::from_secs(1)));
    }
}
