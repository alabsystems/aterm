// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-capacity` — ONE READER OF THE MACHINE AND ONE LEDGER OF CLAIMS ON IT
//! (`docs/DESIGN-adaptive-build-2026-10-24.md`, CAP-1).
//!
//! [`Reading`] is the machine now: cores, load, available and physical memory, swap, and one
//! volume's free space above its floor: the floor the window's disk watch holds it to, from the
//! same knobs and the same measured room ([`floor::Watch`]). The
//! [`Ledger`] holds every session's claims: a tool that needs memory, cores, disk or a quiet
//! machine calls [`admit`] before its work starts and holds the [`Claim`] while it runs. Admit
//! waits, with no deadline, until the need fits beside the claims already held and those that
//! waited longer (first in, first out; a later claim goes first only when it fits beside every
//! earlier one's reservation), printing one line a minute that names the shortfall and each
//! holder. That is admission, not a retry. It refuses at once only a need that can never fit
//! here: all the memory the Mac has or more, or more disk than the volume holds above its floor.
//!
//! A claim's memory reservation is its declared peak less what its holder's process tree
//! already uses, so memory in use is not counted twice: a process is counted toward its
//! nearest claimant ancestor only, and a holder's use is credited once across all its claims.
//! A claim whose work runs outside its holder's tree (a VM, parented to launchd) is credited
//! with those processes ([`Claim::credit`]), whose use counts against it alone.
//! A claim made by a process whose ancestor holds one is that held work going on: it is held to
//! the other held claims only, never to its ancestors' or to a waiter, so a run that holds a
//! claim can start a tool that claims for itself. Other sessions' plain builds hold no claim,
//! but they show in the reading.

#![warn(missing_docs)]

use std::path::PathBuf;

pub mod admit;
pub mod floor;
pub mod footprint;
pub mod ledger;
pub mod reading;
pub mod room;

pub use admit::{
    CREDIT_MAX, Claim, Error, Holder, Pace, Polled, Relief, Short, Shortfall, Waiter, admit,
    admit_relieved, admit_until, admit_with, gib, never, span,
};
pub use footprint::{Estimate, Footprint, Footprints, Meter, allocated, size};
pub use ledger::{Held, Ledger, Record};
#[cfg(any(test, debug_assertions))]
pub use reading::Fixed;
pub use reading::{Live, Machine, Proc, Procs, Reading, TreeUse, Volume, between, cores};

/// This account's home as the account database has it, whatever `$HOME` says.
#[cfg(unix)]
pub(crate) fn account_home() -> Option<PathBuf> {
    aterm_uds::account::home()
}

/// This account's home as the account database has it: none read off Unix.
#[cfg(not(unix))]
pub(crate) const fn account_home() -> Option<PathBuf> {
    None
}

/// Aterm's state root and aterm.toml as the account's own window resolves them: as this
/// process resolves them when it runs as its account's own shell, else (a scratch home, a
/// private state root) in the platform's layout under the account's home, because the window
/// guarding the volume is the account's whatever this process's environment says.
pub(crate) fn own_roots() -> (Option<PathBuf>, Option<PathBuf>) {
    use aterm_types::dirs;
    let account = account_home().filter(|_| dirs::not_own_account().is_some());
    match account {
        None => (dirs::state_dir(), dirs::aterm_config_path()),
        Some(home) => (
            dirs::resolve_state_dir(
                None,
                dirs::StatePlatform {
                    home: Some(home.clone()),
                    ..dirs::StatePlatform::default()
                },
            ),
            dirs::resolve_aterm_config_path(None, None, Some(home.as_os_str())),
        ),
    }
}

/// What a claim needs. Sizes are bytes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Need {
    /// What the work is, as a person reads it in a wait line: `trust check aterm-render`.
    pub what: String,
    /// Its peak memory.
    pub mem: u64,
    /// Memory that must stay available beside its peak for it to be admitted (a VM's host margin): asked of the room, never reserved, so needs admitted side by side share one margin.
    pub margin: u64,
    /// The cores it would use; it is granted as many as are idle, at least one, never more ([`Claim::cores`]). `0`: none asked.
    pub cores: u32,
    /// The disk it will write, on the volume holding [`Self::path`].
    pub disk: u64,
    /// Admit it only while the 1-minute load is at or below this, and no other quiet claim is held or waits ahead.
    pub quiet: Option<f64>,
    /// Where its disk need lands (the working directory when unset). In a [`Record`], the mount of that volume.
    pub path: Option<PathBuf>,
    /// The checkout it runs for, where that is not its working directory's (a tool that confines its working directory to `/`); unset, the working directory's enclosing checkout. A [`Record`] keeps it as its `checkout`, never here.
    pub checkout: Option<String>,
}

impl Need {
    /// A need of nothing yet, for `what`.
    #[must_use]
    pub fn new(what: impl Into<String>) -> Need {
        Need {
            what: what.into(),
            ..Need::default()
        }
    }

    /// Its peak memory, `bytes`.
    #[must_use]
    pub const fn mem(mut self, bytes: u64) -> Need {
        self.mem = bytes;
        self
    }

    /// Its peak memory, `gib` GiB.
    #[must_use]
    pub const fn mem_gib(self, gib: u64) -> Need {
        self.mem(gib.saturating_mul(floor::GIB))
    }

    /// Admit it only while `bytes` more than its peak stays available beside it, reserving none of them ([`Self::margin`]).
    #[must_use]
    pub const fn margin(mut self, bytes: u64) -> Need {
        self.margin = bytes;
        self
    }

    /// The cores it would use.
    #[must_use]
    pub const fn cores(mut self, cores: u32) -> Need {
        self.cores = cores;
        self
    }

    /// The disk it will write, `bytes`, on the volume holding `on`.
    #[must_use]
    pub fn disk(mut self, bytes: u64, on: impl Into<PathBuf>) -> Need {
        self.disk = bytes;
        self.path = Some(on.into());
        self
    }

    /// Admit it only while the 1-minute load is at or below `max_load1`, alone among quiet claims:
    /// unless its own process, or one its process descends from, already holds a quiet claim,
    /// which covers it, so it is admitted under that claim with no wait for either.
    #[must_use]
    pub const fn quiet(mut self, max_load1: f64) -> Need {
        self.quiet = Some(max_load1);
        self
    }

    /// Timing-sensitive work, `what`: a [`Need::quiet`] claim whose bound is [`quiet_bound`] of
    /// this machine's cores.
    #[must_use]
    pub fn timing(what: impl Into<String>) -> Need {
        Need::new(what).quiet(quiet_bound(cores()))
    }

    /// The checkout it runs for, `dir`, which its record and every wait line name in place of the working directory's ([`Self::checkout`]).
    #[must_use]
    pub fn checkout(mut self, dir: impl Into<String>) -> Need {
        self.checkout = Some(dir.into());
        self
    }
}

/// The 1-minute load, per logical core, at or below which the machine is quiet enough for
/// timing-sensitive work ([`Need::timing`]). Measured with `verify measure` on a 14-core M4 Pro
/// (`docs/measured/quiet-admit-2026-10-25.md`): no run whose load averaged 2.65 a core or less
/// starved a take, and a clean run's load averaged at most 0.5 a core above the reading it was
/// admitted at.
pub const QUIET_LOAD_PER_CORE: f64 = 2.0;

/// The most 1-minute load timing-sensitive work starts under on a machine of `cores` logical
/// cores: [`QUIET_LOAD_PER_CORE`] a core.
#[must_use]
pub fn quiet_bound(cores: u32) -> f64 {
    QUIET_LOAD_PER_CORE * f64::from(cores.max(1))
}

/// How a caller admits its work: `--admit wait` (the default) or `--admit now`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Admit {
    /// Wait for room ([`admit`]).
    #[default]
    Wait,
    /// Take the claim at once, room or not, and say what it would have waited for: the owner's override, which keeps a starved run visible as starved.
    Now,
}

impl Admit {
    /// The flag every caller takes: `--admit now` or `--admit wait`.
    pub const FLAG: &'static str = "--admit";

    /// The mode a `--admit` value names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Admit> {
        match word {
            "wait" => Some(Admit::Wait),
            "now" => Some(Admit::Now),
            _ => None,
        }
    }

    /// The word for this mode.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Admit::Wait => "wait",
            Admit::Now => "now",
        }
    }
}

#[cfg(test)]
mod tests;
