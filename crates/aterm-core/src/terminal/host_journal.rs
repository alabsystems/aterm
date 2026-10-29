// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Engine host-mutation journal ([`HostOp`], [`HostReport`]) for the temporal spine.

use super::{ClockReading, HolderCut, Terminal};

/// How many host mutations the journal keeps ([`Terminal::host_journal_since`]).
pub const HOST_JOURNAL_CAP: usize = 32;

const CAP_U64: u64 = HOST_JOURNAL_CAP as u64;

/// One post-seed host-side mutation applied to [`Terminal`] outside `process_at` (plus a `program_probe` foreground transition observed during an unsigned OSC 7).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HostOp {
    /// Foreground-job / probed-job state changed, optionally restoring `cwd_before_job`.
    ForegroundJob {
        /// Whether a foreground process group other than the shell holds the terminal (`set_foreground_job`).
        active: bool,
        /// Whether `program_probe` observed an in-group child running since prompt while `active` is `false`.
        probed: bool,
        /// Whether `set_foreground_job(false)` restored `cwd_before_job`.
        restore_cwd: bool,
    },
    /// A foreground holder cut ([`Terminal::note_holder_cut`]).
    HolderCut(HolderCut),
    /// A stopped drawer's mark retired ([`Terminal::retire_holder_mark`]).
    HolderRetire(u64),
    /// Forced return to the primary screen ([`Terminal::leave_alternate_screen`]).
    LeaveAlternateScreen,
    /// Host policy expects a forwarded `?1049l` ([`Terminal::expect_host_alt_screen_leave`]).
    ExpectHostAltLeave,
    /// Scrollback cleared by the host ([`Terminal::clear_scrollback`]).
    ClearScrollback,
    /// Full terminal reset by the host ([`Terminal::reset`]).
    Reset,
    /// Shell-integration nonce authorized ([`Terminal::authorize_shell_integration`]).
    AuthorizeShell([u8; 32]),
    /// Adopted shell-integration nonce authorized pending first mark ([`Terminal::authorize_shell_integration_on_first_mark`]).
    AuthorizeShellOnFirstMark([u8; 32]),
    /// Typed shell-integration re-key authorized ([`Terminal::authorize_shell_integration_rekey`]).
    AuthorizeShellRekey([u8; 32]),
    /// Typed shell-integration re-key withdrawn ([`Terminal::withdraw_shell_integration_rekey`]).
    WithdrawShellRekey([u8; 32]),
    /// Typed shell-integration re-key settled ([`Terminal::settle_shell_integration_rekey`]).
    SettleShellRekey([u8; 32]),
    /// Autonomous Kitty animation frame step ([`Terminal::advance_kitty_animations`]).
    AdvanceKittyAnimations,
}

impl core::fmt::Debug for HostOp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ForegroundJob {
                active,
                probed,
                restore_cwd,
            } => f
                .debug_struct("ForegroundJob")
                .field("active", active)
                .field("probed", probed)
                .field("restore_cwd", restore_cwd)
                .finish(),
            Self::HolderCut(cut) => f.debug_tuple("HolderCut").field(cut).finish(),
            Self::HolderRetire(token) => f.debug_tuple("HolderRetire").field(token).finish(),
            Self::LeaveAlternateScreen => f.write_str("LeaveAlternateScreen"),
            Self::ExpectHostAltLeave => f.write_str("ExpectHostAltLeave"),
            Self::ClearScrollback => f.write_str("ClearScrollback"),
            Self::Reset => f.write_str("Reset"),
            Self::AuthorizeShell(_) => f.write_str("AuthorizeShell(..)"),
            Self::AuthorizeShellOnFirstMark(_) => f.write_str("AuthorizeShellOnFirstMark(..)"),
            Self::AuthorizeShellRekey(_) => f.write_str("AuthorizeShellRekey(..)"),
            Self::WithdrawShellRekey(_) => f.write_str("WithdrawShellRekey(..)"),
            Self::SettleShellRekey(_) => f.write_str("SettleShellRekey(..)"),
            Self::AdvanceKittyAnimations => f.write_str("AdvanceKittyAnimations"),
        }
    }
}

impl Default for HostOp {
    #[inline]
    fn default() -> Self {
        Self::ForegroundJob {
            active: false,
            probed: false,
            restore_cwd: false,
        }
    }
}

impl HostOp {
    /// Whether this mutation visibly alters the screen or scrollback on its own, warranting a distinct steppable Rewind instant.
    #[inline]
    #[must_use]
    pub const fn is_screen_cut(&self) -> bool {
        matches!(
            self,
            Self::LeaveAlternateScreen
                | Self::ClearScrollback
                | Self::Reset
                | Self::AdvanceKittyAnimations
        )
    }
}

/// One host mutation stamped with its 1-based ordinal in this engine's lifetime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostReport {
    /// 1-based position in this engine's host-mutation sequence.
    pub ordinal: u64,
    /// The mutation applied.
    pub op: HostOp,
    /// Clock reading at which the mutation ran, when time-sensitive (`LeaveAlternateScreen`).
    /// `AdvanceKittyAnimations` carries its monotonic frame reading as well.
    pub clock: Option<ClockReading>,
}

/// Ring of the newest [`HOST_JOURNAL_CAP`] host mutations on a [`Terminal`].
#[derive(Clone, Debug)]
pub(super) struct HostJournal {
    ordinal: u64,
    ring: [HostReport; HOST_JOURNAL_CAP],
}

impl Default for HostJournal {
    #[inline]
    fn default() -> Self {
        Self {
            ordinal: 0,
            ring: [HostReport::default(); HOST_JOURNAL_CAP],
        }
    }
}

#[inline]
#[allow(
    clippy::cast_possible_truncation,
    reason = "CAP_U64 is 32, so modulo CAP_U64 always fits in usize"
)]
const fn slot(ordinal: u64) -> usize {
    (ordinal.wrapping_sub(1) % CAP_U64) as usize
}

impl HostJournal {
    /// Record `op` as the next host mutation and stamp its ordinal.
    #[inline]
    pub(super) const fn push(&mut self, op: HostOp) {
        self.push_inner(op, None);
    }

    /// Record time-sensitive `op` with `clock` as the next host mutation and stamp its ordinal.
    #[inline]
    pub(super) const fn push_at(&mut self, op: HostOp, clock: ClockReading) {
        self.push_inner(op, Some(clock));
    }

    #[inline]
    const fn push_inner(&mut self, op: HostOp, clock: Option<ClockReading>) {
        self.ordinal = self.ordinal.wrapping_add(1);
        self.ring[slot(self.ordinal)] = HostReport {
            ordinal: self.ordinal,
            op,
            clock,
        };
    }

    /// Reports after `since` and the overrun count ([`Terminal::host_journal_since`]).
    fn since(&self, since: u64) -> (Vec<HostReport>, u64) {
        let since = if since > self.ordinal { 0 } else { since };
        let pending = self.ordinal - since;
        let kept = pending.min(CAP_U64);
        let reports = (self.ordinal - kept + 1..=self.ordinal)
            .filter_map(|o| self.ring.get(slot(o)).copied())
            .collect();
        (reports, pending - kept)
    }
}

impl Terminal {
    /// Monotonic count of host-side mutations recorded on this engine.
    #[inline]
    #[must_use]
    pub const fn host_ordinal(&self) -> u64 {
        self.host_journal.ordinal
    }

    /// Host mutations recorded after `ordinal`, oldest first (at most [`HOST_JOURNAL_CAP`]), plus how many earlier ones were overwritten.
    #[must_use]
    pub fn host_journal_since(&self, ordinal: u64) -> (Vec<HostReport>, u64) {
        self.host_journal.since(ordinal)
    }

    /// Replay a recorded [`HostOp`] at a caller-supplied clock reading.
    pub fn apply_host_op_at(&mut self, op: HostOp, clock: ClockReading) {
        match op {
            HostOp::ForegroundJob {
                active,
                probed,
                restore_cwd,
            } => {
                let prev = (self.shell.foreground_job, self.shell.probe_job_active);
                self.shell.foreground_job = active;
                self.shell.probe_job_active = !active && probed;
                let mut restored = false;
                if !active
                    && !probed
                    && restore_cwd
                    && let Some(before) = self.shell.cwd_before_job.take()
                {
                    restored = true;
                    let (_, mut handler) = self.split_for_process();
                    handler.apply_cwd_report(before);
                }
                let after = (self.shell.foreground_job, self.shell.probe_job_active);
                if after != prev || restored {
                    self.host_journal.push(HostOp::ForegroundJob {
                        active: after.0,
                        probed: after.1,
                        restore_cwd: restored,
                    });
                }
            }
            HostOp::HolderCut(cut) => self.note_holder_cut(cut),
            HostOp::HolderRetire(token) => self.retire_holder_mark(token),
            HostOp::LeaveAlternateScreen => {
                let _ = self.leave_alternate_screen_at(clock);
            }
            HostOp::ExpectHostAltLeave => {
                let _ = self.expect_host_alt_screen_leave();
            }
            HostOp::ClearScrollback => self.clear_scrollback(),
            HostOp::Reset => self.reset(),
            HostOp::AuthorizeShell(nonce) => self.authorize_shell_integration(nonce),
            HostOp::AuthorizeShellOnFirstMark(nonce) => {
                self.authorize_shell_integration_on_first_mark(nonce);
            }
            HostOp::AuthorizeShellRekey(nonce) => self.authorize_shell_integration_rekey(nonce),
            HostOp::WithdrawShellRekey(nonce) => {
                let _ = self.withdraw_shell_integration_rekey(&nonce);
            }
            HostOp::SettleShellRekey(nonce) => {
                let _ = self.settle_shell_integration_rekey(&nonce);
            }
            HostOp::AdvanceKittyAnimations => {
                let _ = self.advance_kitty_animations(clock.monotonic);
            }
        }
    }

    /// Replay a recorded [`HostOp`] reading the live clock when needed.
    #[inline]
    pub fn apply_host_op(&mut self, op: HostOp) {
        self.apply_host_op_at(op, ClockReading::now());
    }
}
