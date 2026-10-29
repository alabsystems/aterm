// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-capacity` — ONE READER OF THE MACHINE AND ONE LEDGER OF CLAIMS ON IT
//! (`docs/DESIGN-adaptive-build-2026-10-24.md`, CAP-1).
//!
//! [`Reading`] is the machine now: cores, load, available, wired and physical memory, swap,
//! and one volume's free space above its floor: the floor the window's disk watch holds it
//! to, from the same knobs and the same measured room ([`floor::Watch`]). The
//! [`Ledger`] holds every session's claims: a tool that needs memory, cores, disk or a quiet
//! machine calls [`admit`] before its work starts and holds the [`Claim`] while it runs. Admit
//! waits, with no deadline, until the need fits beside the claims already held and those that
//! waited longer (first in, first out; a later claim goes first only when it fits beside every
//! earlier one's reservation), printing one line a minute that names the shortfall and each
//! holder. That is admission, not a retry. An earlier waiter whose disk need is more than the
//! volume has free above its floor now reserves none of it ([`Short::unservable`]): what a
//! claim wrote stays on the volume once it ends, so no claim's end can make room for it, only
//! space freed, and the waiters behind it go first while it waits for that, so it is not kept
//! from being overtaken (the trade, in [`mod@admit`]). A memory need with no margin that does
//! not fit, where no other claim holds memory or waits ahead for it, is admitted ALONE: what it
//! is short of is work no claim holds, whose end no claim's end brings, so a wait for it would
//! have no bound; it runs with no other memory claim beside it, each waiting while it is held
//! ([`Short::Alone`]). It refuses at once only a need that can never fit here: all the memory
//! that can ever be available or more (physical memory less what is wired,
//! [`Reading::reachable`]) unless it asks to be admitted alone ([`Need::alone`]), or more disk
//! than the volume holds above its floor.
//! Timing-sensitive work waits for the load within its [`Patience`]: past its reservation age
//! it holds back claimed work that asks after it, and at its bound it starts whatever the load.
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
use std::time::Duration;

pub mod admit;
pub mod floor;
pub mod footprint;
pub mod ledger;
pub mod reading;
pub mod room;

pub use admit::{
    Alone, CREDIT_MAX, Claim, Error, Holder, Pace, Polled, QuietStart, Relief, Reserve, Short,
    Shortfall, Waiter, admit, admit_relieved, admit_until, admit_with, gib, never, reserved, span,
};
pub use footprint::{Estimate, Footprint, Footprints, Meter, allocated, allocated_until, size};
pub use ledger::{Held, Ledger, Record};
#[cfg(any(test, debug_assertions))]
pub use reading::Fixed;
pub use reading::{
    Live, Machine, Proc, Procs, Reading, TreeUse, Volume, between, claimable, cores,
};

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
    /// The cores it would use; it is granted its share of them, at least one, never more, and never waits for them ([`Claim::cores`]). `0`: none asked.
    pub cores: u32,
    /// The disk it will write, on the volume holding [`Self::path`].
    pub disk: u64,
    /// Admit it only while the 1-minute load is at or below this, and no other quiet claim is held or waits ahead.
    pub quiet: Option<f64>,
    /// How long a quiet claim waits for the load ([`Patience`]); unset, it waits until the load falls.
    pub patience: Option<Patience>,
    /// Where its disk need lands (the working directory when unset). In a [`Record`], the mount of that volume.
    pub path: Option<PathBuf>,
    /// The checkout it runs for, where that is not its working directory's (a tool that confines its working directory to `/`); unset, the working directory's enclosing checkout. A [`Record`] keeps it as its `checkout`, never here.
    pub checkout: Option<String>,
    /// Admit its memory only ALONE ([`Self::alone`]): once no other claim holds memory or waits ahead for it, whatever is available, and while it is held no other memory claim is admitted.
    pub alone: bool,
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

    /// Wait for the load within `patience` ([`Patience`]), for a [`Self::quiet`] claim.
    #[must_use]
    pub const fn patience(mut self, patience: Patience) -> Need {
        self.patience = Some(patience);
        self
    }

    /// Timing-sensitive work, `what`: a [`Need::quiet`] claim whose bound is [`quiet_bound`] of
    /// this machine's cores, waiting for it within [`QUIET_PATIENCE`].
    #[must_use]
    pub fn timing(what: impl Into<String>) -> Need {
        Need::new(what)
            .quiet(quiet_bound(cores()))
            .patience(QUIET_PATIENCE)
    }

    /// The checkout it runs for, `dir`, which its record and every wait line name in place of the working directory's ([`Self::checkout`]).
    #[must_use]
    pub fn checkout(mut self, dir: impl Into<String>) -> Need {
        self.checkout = Some(dir.into());
        self
    }

    /// Admit its memory only ALONE: once no other claim on the machine holds memory or waits
    /// ahead for it, however much is available, and while it is held every other memory claim
    /// waits ([`Short::Alone`]). What work that needs more memory than can ever be available here
    /// asks: [`never`] refuses such a need otherwise, and admitted beside other claims it would
    /// overrun the machine with them. A memory need with no margin that is short only of memory
    /// no claim holds is admitted alone without asking (the ledger's ALONE rule, [`mod@admit`]).
    #[must_use]
    pub const fn alone(mut self) -> Need {
        self.alone = true;
        self
    }
}

/// A memory claim made from a measured peak adds a fifth of it ([`peak_claim`]): one library's
/// recorded Trust check peaks moved by 14% between two runs on one prover (aterm-render, 13.2 GB
/// on 2026-10-22 and 15.0 GB on 2026-10-21).
pub const PEAK_MARGIN_DIV: u64 = 5;

/// The least margin a memory claim adds to a measured peak ([`peak_claim`]): 1 GiB.
pub const PEAK_MARGIN_MIN: u64 = floor::GIB;

/// THE MEMORY WORK WHOSE MEASURED PEAK IS `peak` BYTES CLAIMS: that peak, and a margin of a fifth
/// of it ([`PEAK_MARGIN_DIV`]) and at least [`PEAK_MARGIN_MIN`], saturating. The one rule the
/// Trust lane's checks (their cost history's largest process) and verify's stages (their last
/// runs' largest peak) claim by. A claim is never cut to fit the machine: one that can never fit
/// is its caller's to refuse or bound ([`never`]).
#[must_use]
pub const fn peak_claim(peak: u64) -> u64 {
    let margin = peak / PEAK_MARGIN_DIV;
    peak.saturating_add(if margin > PEAK_MARGIN_MIN {
        margin
    } else {
        PEAK_MARGIN_MIN
    })
}

/// What heavy work never measured here claims at most ([`cold_claim`]): 16 GiB.
pub const COLD_CLAIM: u64 = 16 << 30;

/// THE MEMORY HEAVY WORK NEVER MEASURED HERE CLAIMS, on a machine of `physical` bytes of memory
/// (`None`: unread): [`COLD_CLAIM`], and never more than half of physical memory, so a host of
/// any size can admit work it has not measured. The Trust lane's check of a library with no
/// recorded peak claims it, and so does each verify stage that compiles, until a run of it is
/// measured.
#[must_use]
pub const fn cold_claim(physical: Option<u64>) -> u64 {
    match physical {
        Some(p) if p > 0 && p / 2 < COLD_CLAIM => p / 2,
        _ => COLD_CLAIM,
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

/// How long a quiet claim waits for the load once nothing but the load holds it back (no other
/// quiet claim is held or waits ahead of it: it is next). Its patience counts from then, so a
/// claim that queued behind another timed run still gets all of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Patience {
    /// Next for this long, it reserves the machine: claimed work that asked after it (memory,
    /// cores or disk, outside its own process tree and ancestry) waits, naming it, so no new
    /// grant or admission adds load while it waits.
    pub reserve: Duration,
    /// Next for this long, it starts whatever the load: its bound. Its tool's report says the load
    /// it started under.
    pub start: Duration,
    /// Its reservation lasts this long into the work once it starts, at most, so what it held back
    /// does not start in a burst beside the work's first minutes; it ends sooner when the claim
    /// does. A reservation therefore lasts at most `start - reserve + hold`.
    pub hold: Duration,
}

/// The patience of timing-sensitive work ([`Need::timing`]), from 13 hours of this 14-core M4
/// Pro's 1-minute load (`docs/measured/quiet-bound-2026-10-25.md`), above the quiet bound half
/// the time: 26 of its 58 spells above the bound ended within 2 minutes, so it reserves after
/// 2; 65% of moments above the bound reached a quiet machine within 10 minutes, and one still
/// waiting at 10 had a median 11 minutes more to go, so it starts by 10; its reservation holds
/// 15 minutes into the work, past the longest measuring stage QUIET-ADMIT ran under load
/// (771 s).
pub const QUIET_PATIENCE: Patience = Patience {
    reserve: Duration::from_secs(120),
    start: Duration::from_secs(600),
    hold: Duration::from_secs(900),
};

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

#[cfg(all(test, unix))]
mod interrupt_tests;
