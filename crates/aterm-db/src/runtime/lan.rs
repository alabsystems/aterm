// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! When the Commons runtime may make its first LAN syscall. macOS asks its Local Network question
//! at an app's first LAN use (a multicast bind or join, a send, a listener or a dial), so that use
//! decides when the alert comes up. System State's Local Network step is the one meant to raise it,
//! in its step order and never beside another macOS question
//! (`docs/DESIGN-system-state-2026-10-21.md` §6). A [`LanGate`] holds the owner before it opens
//! its observer or any socket until its host says the LAN is the runtime's, and its host may
//! defer that first use itself ([`LanGate::before_use`]): an open owner may make none for a while
//! (another process owns the home's observer, or its Commons state errs before its transport),
//! and the question comes at the use. Once open it never closes: the first use is the one that
//! puts the OS's question to this app on this Mac, if the OS never asked it, and a later use
//! raises none; where the OS refused, the transport's next refresh after the person allows it
//! goes through.

use super::Status;
use aterm_types::network_state::Visibility;
use std::path::Path;

/// One look at whether the runtime may use the LAN, with the reason it says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LanTurn {
    /// It may, from now on in this runtime: why.
    Open(&'static str),
    /// Not yet: what it waits for, which the observation shows as its detail.
    Held(&'static str),
}

/// The owner thread's gate before its first LAN use. Once open, it stays open for the runtime.
pub struct LanGate {
    opened: bool,
    turn: Option<Box<dyn FnMut() -> LanTurn + Send>>,
    refused: Option<Box<dyn Fn() -> bool + Send>>,
    ready: Option<Box<dyn Fn() -> Option<&'static str> + Send>>,
    keep_use: Option<Box<dyn Fn() -> bool + Send>>,
    missed: Option<Box<dyn Fn() + Send>>,
}

impl LanGate {
    /// A gate that never holds: off macOS, where no question guards the LAN, and in tests.
    #[must_use]
    pub fn open() -> Self {
        Self {
            opened: false,
            turn: None,
            refused: None,
            ready: None,
            keep_use: None,
            missed: None,
        }
    }

    /// A gate that asks `turn` at each look of the owner thread until it says [`LanTurn::Open`].
    /// `turn` runs on the owner thread, about every 200 ms in a window and every 2 s in the
    /// background, so it keeps its own pace for anything costly.
    pub fn asking(turn: impl FnMut() -> LanTurn + Send + 'static) -> Self {
        Self {
            opened: false,
            turn: Some(Box::new(turn)),
            refused: None,
            ready: None,
            keep_use: None,
            missed: None,
        }
    }

    /// This gate, with `refused` saying whether the host's records hear that the OS refused the
    /// runtime the LAN (macOS's Local Network switch off for aterm). An open owner reads it at
    /// each poll, only to say so in its observation while its transport's links all failed: it
    /// never closes the LAN.
    #[must_use]
    pub fn with_refusal(mut self, refused: impl Fn() -> bool + Send + 'static) -> Self {
        self.refused = Some(Box::new(refused));
        self
    }

    /// Whether the host's records hear that the OS refused the runtime the LAN; false for a gate
    /// with no reader ([`Self::with_refusal`]).
    pub(super) fn refused(&self) -> bool {
        self.refused.as_ref().is_some_and(|refused| refused())
    }

    /// This gate, with `ready` asked right before the runtime's first LAN use (its first transport
    /// open or refresh on a non-loopback address), at each try until its use is kept
    /// ([`Self::keeping_use`]): `None` lets the use go now; `Some(why)` defers it, and the owner
    /// waits with `why` as its observation's detail and tries again. The OS's question comes at
    /// that use, not when the gate opens, so a host that guards the question (holds a dialog slot
    /// around it) does so here. Each use `ready` lets go ends right after in the keeper (it reached
    /// the LAN) or in the miss ([`Self::missing_use`]). A try with no candidate address but a
    /// loopback one (the runtime reads IPv4 addresses only) is no use and asks nothing.
    #[must_use]
    pub fn before_use(mut self, ready: impl Fn() -> Option<&'static str> + Send + 'static) -> Self {
        self.ready = Some(Box::new(ready));
        self
    }

    /// What the host waits for before the runtime's first LAN use ([`Self::before_use`]); `None`,
    /// the use may go now, for a gate with no reader.
    pub(super) fn ready_for_use(&self) -> Option<&'static str> {
        self.ready.as_ref().and_then(|ready| ready())
    }

    /// This gate, with `keep` run right after the runtime made a LAN use: it keeps, for the host's
    /// later runtimes, that this one made it (and starts whatever the host holds from the use), and
    /// says whether it is done. The owner runs it on its first transport open or refresh on a
    /// non-loopback address that reached the LAN (a socket bound, or failed only once it was
    /// made, after a syscall at which the OS may have asked), and again at the next until it says
    /// it is done. A host keeps a use only where the OS's question that use raises is its own
    /// app's on its own Mac; the runtime keeps no record of its own.
    #[must_use]
    pub fn keeping_use(mut self, keep: impl Fn() -> bool + Send + 'static) -> Self {
        self.keep_use = Some(Box::new(keep));
        self
    }

    /// This gate, with `missed` run right after a use [`Self::before_use`] let go reached no LAN
    /// socket: the transport failed to open, or every bind refused at its checks, before its
    /// socket (an attachment that changed since its address was read), so the OS asked nothing. A
    /// host that holds something for the use lets it go here; the next try asks `ready` again. A
    /// try with no candidate address but a loopback one (no IPv4 address: the runtime reads IPv4
    /// addresses only) asks no `ready` and makes no use, so it runs no `missed` either: what the
    /// host holds for the use goes by the host's own wait.
    #[must_use]
    pub fn missing_use(mut self, missed: impl Fn() + Send + 'static) -> Self {
        self.missed = Some(Box::new(missed));
        self
    }

    /// The runtime's LAN use, right after it opened or refreshed its transport on a non-loopback
    /// address: where it reached the LAN (`bound`), kept with the host's keeper
    /// ([`Self::keeping_use`]), and whether it is (true for a gate with none: nothing to keep);
    /// where it reached none, no use, which the host hears ([`Self::missing_use`]), and false.
    pub(super) fn used(&self, bound: bool) -> bool {
        if bound {
            return self.keep_use.as_ref().is_none_or(|keep| keep());
        }
        if let Some(missed) = &self.missed {
            missed();
        }
        false
    }

    /// The host's turn at one look of the owner thread, `None` for a gate that asks none (a
    /// host's wiring tests).
    #[cfg(any(test, debug_assertions))]
    pub fn look_now(&mut self) -> Option<LanTurn> {
        self.turn.as_mut().map(|turn| turn())
    }

    /// One try of the runtime's first LAN use as the owner makes it, with whether the transport
    /// `bound` a LAN socket (a host's wiring tests): `Err(why)` where the host defers it
    /// ([`Self::before_use`]), else whether the use is kept ([`Self::used`]).
    #[cfg(any(test, debug_assertions))]
    pub fn use_now(&self, bound: bool) -> std::result::Result<bool, &'static str> {
        match self.ready_for_use() {
            Some(why) => Err(why),
            None => Ok(self.used(bound)),
        }
    }

    /// What holds the owner before its observer and transport open, as the observation's
    /// visibility and detail and the status it reports; `None` once the LAN is the runtime's. A
    /// disabled or unreadable Commons configuration gives the visibility, detail and status an
    /// open owner gives it, without asking the gate: nothing is waited for there. The owner
    /// publishes what holds it and serves it to other local processes on its wait relay
    /// (`network_state::WaitRelay`), since it opened no observer whose relay would.
    pub(super) fn held(&mut self, config: &Path) -> Option<(Visibility, &'static str, Status)> {
        if self.opened {
            return None;
        }
        match super::enabled(config) {
            Err(_) => {
                return Some((
                    Visibility::Unavailable,
                    "Network observation unavailable",
                    Status::Refused,
                ));
            }
            Ok(false) => {
                return Some((
                    Visibility::Disabled,
                    "Network visibility disabled",
                    Status::Disabled,
                ));
            }
            Ok(true) => {}
        }
        match self.turn.as_mut().map(|turn| turn()) {
            None | Some(LanTurn::Open(_)) => {
                self.opened = true;
                None
            }
            Some(LanTurn::Held(detail)) => Some((Visibility::Waiting, detail, Status::LanHeld)),
        }
    }
}

impl Default for LanGate {
    fn default() -> Self {
        Self::open()
    }
}

impl std::fmt::Debug for LanGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LanGate")
            .field("opened", &self.opened)
            .field("asking", &self.turn.is_some())
            .field("hears_refusal", &self.refused.is_some())
            .field("readies_use", &self.ready.is_some())
            .field("keeps_use", &self.keep_use.is_some())
            .field("misses_use", &self.missed.is_some())
            .finish()
    }
}
