// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The machine's situation as the OS tells of it when it changes (stats deck A4 §8,
//! `docs/DESIGN-stats-deck-instruments-2026-11-06.md`): the lid, the power source and the
//! battery, the external displays, and the last sleep with its cause. The machine's type is
//! [`crate::machine_type`]'s; its network place is netplace's.
//!
//! Each reader reads on change and costs nothing at rest:
//!
//! - **Power** ([`PowerWatch`]): two notify check tokens, the power source's
//!   (`com.apple.system.powersources.source`, posted on a switch) and the charge's
//!   (`com.apple.system.powersources.percent`, posted as the charge moves). A reading checks
//!   both (a check wakes nothing) and reads the power sources (`IOPSCopyPowerSourcesInfo`) only
//!   when one was posted, and the adapter's rating only when the source's was ([`PowerLatch`]).
//! - **Lid and displays** ([`Watch`]): one thread whose run loop holds the power manager's root
//!   domain's general interest, whose clamshell message carries the lid's closed bit (the
//!   message itself is not a lid change: attaching AC sends it with only the sleep bit set,
//!   [`LidLatch`]), and the matching notifications of the display pipes macOS publishes for
//!   each display (`DCPAVServiceProxy`, its `Location` `External` for an external one, on
//!   Apple silicon). It holds no timer: it wakes for a message and for nothing else.
//! - **Sleep** ([`WatchHandle::woke`]): the host's sleep and wake listener
//!   (`aterm_uds::power::listen`) calls it at each wake, which reads the kernel's last sleep and
//!   wake times and the power manager's reason for that sleep (`Last Sleep Reason`, which it
//!   keeps for the last sleep only), and the lid again.
//!
//! On Windows the power source and the battery are read (`GetSystemPowerStatus`, at each
//! reading: [`crate::power::windows`]); the lid, the displays and the last sleep have no reader
//! there yet. On Linux there is no reader for any of them ([`LID_READER`] and its siblings say
//! so), and every value is `None`.

use std::sync::{Arc, Mutex, PoisonError};

/// Whether this OS has a lid reader.
pub const LID_READER: bool = cfg!(target_os = "macos");
/// Whether this OS has a power-source reader.
pub const POWER_READER: bool = cfg!(any(target_os = "macos", windows));
/// Whether this OS has an external-display reader: Apple silicon's display pipes. An Intel Mac
/// publishes none, so it has no reader rather than a count that always reads zero.
pub const DISPLAY_READER: bool = cfg!(all(target_os = "macos", target_arch = "aarch64"));
/// Whether this OS has a sleep reader.
pub const SLEEP_READER: bool = cfg!(target_os = "macos");

/// `kClamshellStateBit` (`IOKit/pwr_mgt/IOPM.h`): the lid is closed.
pub const CLAMSHELL_CLOSED_BIT: usize = 1 << 0;
/// `kClamshellSleepBit`: closing the lid would sleep the machine now.
pub const CLAMSHELL_SLEEP_BIT: usize = 1 << 1;

/// The power manager's notify name for a power-source switch (`kIOPSNotifyPowerSource`).
pub const NOTIFY_POWER_SOURCE: &std::ffi::CStr = c"com.apple.system.powersources.source";
/// The power manager's notify name for a change of the battery's charge
/// (`kIOPSNotifyPercentChange`, which the public header does not carry; N1 measured it firing as
/// the charge moved).
pub const NOTIFY_CHARGE: &std::ffi::CStr = c"com.apple.system.powersources.percent";

/// The unix time now, ms.
#[cfg(target_os = "macos")]
pub(crate) fn unix_ms_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The lid, as the power manager says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lid {
    /// Open.
    Open,
    /// Closed.
    Closed,
    /// No lid: the root domain publishes no `AppleClamshellState` ("not present == no clamshell
    /// on this hardware", `IOPM.h`).
    None,
}

/// The lid and when it last changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LidState {
    /// Open, closed or none.
    pub lid: Lid,
    /// When the closed bit last changed while this process heard it (unix ms); `None` until it
    /// changed after the first reading.
    pub changed_ms: Option<i64>,
}

/// The lid as the readings and messages so far say it. A change of the lid is the closed bit
/// changing, never a message: the root domain sends its clamshell message when either of its
/// two bits moves, and attaching AC sent it with the sleep bit alone (N1, `n1/queue.md`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LidLatch {
    state: Option<LidState>,
}

impl LidLatch {
    /// The lid as last read or heard; `None` until the first.
    #[must_use]
    pub const fn state(&self) -> Option<LidState> {
        self.state
    }

    /// A registry reading of `AppleClamshellState`: `Some(closed)`, or `None` where the root
    /// domain publishes none (no lid). Whether the lid changed.
    pub fn read(&mut self, closed: Option<bool>, now_ms: i64) -> bool {
        self.set(
            match closed {
                Some(true) => Lid::Closed,
                Some(false) => Lid::Open,
                None => Lid::None,
            },
            now_ms,
        )
    }

    /// A clamshell message's argument `bits` ([`CLAMSHELL_CLOSED_BIT`],
    /// [`CLAMSHELL_SLEEP_BIT`]): the lid is what its closed bit says; the sleep bit is not the
    /// lid. Whether the lid changed.
    pub fn clamshell(&mut self, bits: usize, now_ms: i64) -> bool {
        self.set(
            if bits & CLAMSHELL_CLOSED_BIT == 0 {
                Lid::Open
            } else {
                Lid::Closed
            },
            now_ms,
        )
    }

    fn set(&mut self, lid: Lid, now_ms: i64) -> bool {
        match self.state {
            Some(held) if held.lid == lid => false,
            Some(held) if held.lid != Lid::None && lid != Lid::None => {
                self.state = Some(LidState {
                    lid,
                    changed_ms: Some(now_ms),
                });
                true
            }
            _ => {
                self.state = Some(LidState {
                    lid,
                    changed_ms: None,
                });
                false
            }
        }
    }
}

/// What powers the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerSource {
    /// A wall adapter (`AC Power`).
    Ac,
    /// The internal battery (`Battery Power`).
    Battery,
    /// A UPS's battery (`UPS Power`).
    Ups,
}

impl PowerSource {
    /// The source `IOPSGetProvidingPowerSourceType` names (`kIOPMACPowerKey`,
    /// `kIOPMBatteryPowerKey`, `kIOPMUPSPowerKey`); `None` for any other word.
    #[must_use]
    pub fn of(word: &str) -> Option<Self> {
        match word {
            "AC Power" => Some(Self::Ac),
            "Battery Power" => Some(Self::Battery),
            "UPS Power" => Some(Self::Ups),
            _ => None,
        }
    }
}

/// The internal battery's reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryReading {
    /// Charge, 0–100 % (`Current Capacity` of `Max Capacity`, which the power sources give as
    /// a percentage).
    pub pct: u8,
    /// `Is Charging`.
    pub charging: bool,
}

/// One read of the power sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerReading {
    /// What provides power.
    pub source: PowerSource,
    /// The internal battery; `None` where there is none.
    pub battery: Option<BatteryReading>,
    /// The wall adapter's `Watts`; `None` on battery or where it is not reported.
    pub adapter_w: Option<u32>,
}

/// The power sources as last read, and when the source last switched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerState {
    /// The last read.
    pub reading: PowerReading,
    /// When a read first saw the source switch (unix ms); `None` until it switched after the
    /// first read.
    pub changed_ms: Option<i64>,
}

/// Which power notifications were posted since the last reading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Posted {
    /// The power source's ([`NOTIFY_POWER_SOURCE`]): a switch, an adapter attached or gone.
    pub source: bool,
    /// The charge's ([`NOTIFY_CHARGE`]).
    pub charge: bool,
}

/// How long after a failed read of the power sources it is tried again with nothing posted: a
/// read that keeps failing costs a read a minute, never one a reading.
pub const POWER_RETRY_MS: u64 = 60_000;

/// What a reading knows of the power sources: it reads them only when a notification says
/// they changed (or a read failed [`POWER_RETRY_MS`] ago), never because a reading came. The
/// adapter's rating, which costs as much again (`docs/measured/stats-deck-a4-n2-situ-2026-11-07.md`)
/// and changes only with the adapter, is read only where the source's notification was posted
/// (attaching an adapter posts it, and so does swapping one, which passes through battery) or
/// the source became AC; a charge post keeps the rating read before.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PowerLatch {
    reading: Option<PowerReading>,
    last_source: Option<PowerSource>,
    changed_ms: Option<i64>,
    /// When the last read failed (unix ms), until one answers.
    failed_ms: Option<i64>,
    /// A source post whose adapter read has not yet answered with a reading.
    adapter_due: bool,
    /// The adapter's rating as last read on AC.
    adapter_w: Option<u32>,
}

impl PowerLatch {
    /// One reading at `now_ms`, with what was `posted` since the last one. `read` reads the
    /// power sources (without the adapter) and is called only when a notification was posted or
    /// the last call failed [`POWER_RETRY_MS`] ago or more; its failure reads unread until a read
    /// answers. `adapter`
    /// reads the adapter's rating and is called only for a read on AC after a source post, or
    /// when the source became AC.
    pub fn step(
        &mut self,
        posted: Posted,
        now_ms: i64,
        read: impl FnOnce() -> Option<PowerReading>,
        adapter: impl FnOnce() -> Option<u32>,
    ) -> Option<PowerState> {
        self.adapter_due |= posted.source;
        let retry = self
            .failed_ms
            .is_some_and(|failed| now_ms.abs_diff(failed) >= POWER_RETRY_MS);
        if posted.source || posted.charge || retry {
            self.reading = read().map(|mut r| {
                r.adapter_w = match r.source {
                    PowerSource::Ac if self.adapter_due || self.last_source != Some(r.source) => {
                        adapter()
                    }
                    PowerSource::Ac => self.adapter_w,
                    PowerSource::Battery | PowerSource::Ups => None,
                };
                r
            });
            self.failed_ms = self.reading.is_none().then_some(now_ms);
            if let Some(r) = self.reading {
                if self.last_source.is_some_and(|s| s != r.source) {
                    self.changed_ms = Some(now_ms);
                }
                self.last_source = Some(r.source);
                self.adapter_due = false;
                self.adapter_w = r.adapter_w;
            }
        }
        self.reading.map(|reading| PowerState {
            reading,
            changed_ms: self.changed_ms,
        })
    }
}

/// The power sources, read on change: two check tokens a reading checks, and the reads they
/// gate ([`PowerLatch`]). Windows posts no such token to a process without a window, so there
/// every reading reads `GetSystemPowerStatus`, which asks no other process. On Linux it reads
/// nothing.
#[derive(Debug)]
pub struct PowerWatch {
    latch: PowerLatch,
    #[cfg(target_os = "macos")]
    tokens: crate::situation_macos::Tokens,
}

impl PowerWatch {
    /// Registers the check tokens (on macOS; nothing elsewhere).
    #[must_use]
    pub fn new() -> Self {
        Self {
            latch: PowerLatch::default(),
            #[cfg(target_os = "macos")]
            tokens: crate::situation_macos::Tokens::register(),
        }
    }

    /// The power sources at a reading at `now_ms`: the last read, read again first only when a
    /// token says they changed. `None` where unread (no token registered, no read answered, or
    /// off macOS).
    pub fn read(&mut self, now_ms: i64) -> Option<PowerState> {
        #[cfg(target_os = "macos")]
        {
            let posted = self.tokens.posted()?;
            self.latch.step(
                posted,
                now_ms,
                crate::situation_macos::power_reading,
                crate::situation_macos::adapter_watts,
            )
        }
        #[cfg(windows)]
        {
            let posted = Posted {
                source: false,
                charge: true,
            };
            let read = crate::power::windows::power_reading;
            self.latch.step(posted, now_ms, read, || None)
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            self.latch.step(Posted::default(), now_ms, || None, || None)
        }
    }
}

impl Default for PowerWatch {
    fn default() -> Self {
        Self::new()
    }
}

/// One read of the power sources now (`IOPSCopyPowerSourcesInfo`, without the adapter's
/// details): the read a [`PowerWatch`] takes when a token was posted. A reading reads through
/// the watch, never this, so the sources are read only on a notification (A4 §8); this is for
/// a measurement or a check. `None` off macOS or where the power manager answered nothing.
#[must_use]
pub fn power_sources() -> Option<PowerReading> {
    #[cfg(target_os = "macos")]
    {
        crate::situation_macos::power_reading()
    }
    #[cfg(windows)]
    {
        crate::power::windows::power_reading()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        None
    }
}

/// The wall adapter's rating (`IOPSCopyExternalPowerAdapterDetails`' `Watts`), which a
/// [`PowerWatch`] reads beside [`power_sources`] after a source post on AC; for a measurement.
/// `None` off macOS, on battery, or where no rating is given.
#[must_use]
pub fn adapter_watts() -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        crate::situation_macos::adapter_watts()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// External displays attached now, by one listing of the display pipes (what the [`Watch`]'s
/// first matching drain reads as it starts). The watch keeps the count on notifications; this
/// census is for a measurement or a check (M-DOCK). `None` where there is no reader
/// ([`DISPLAY_READER`]) or the listing failed.
#[must_use]
pub fn external_displays() -> Option<u16> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        crate::situation_macos::external_census()
    }
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    {
        None
    }
}

/// One sleep, as the kernel and the power manager record it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SleepSpan {
    /// `kern.sleeptime`, unix ms.
    pub from_ms: i64,
    /// `kern.waketime`, unix ms.
    pub to_ms: i64,
    /// The root domain's `Last Sleep Reason` (`Clamshell Sleep`, `Idle Sleep`).
    pub cause: Option<String>,
    /// The last word of `kern.wakereason` ([`crate::timeline::wake_word`]).
    pub wake: Option<String>,
}

/// The machine's boot and its last sleep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Awake {
    /// `kern.boottime`, unix ms.
    pub boot_ms: i64,
    /// The last sleep since boot; `None` when it has not slept.
    pub last_sleep: Option<SleepSpan>,
}

/// The last sleep from the kernel's timeline and the reason the power manager gives for it:
/// a span only where the wake follows the sleep.
#[must_use]
pub fn awake_of(timeline: &crate::timeline::Timeline, cause: Option<String>) -> Awake {
    let last_sleep = match (timeline.sleep_ms, timeline.wake_ms) {
        (Some(from_ms), Some(to_ms)) if to_ms > from_ms => Some(SleepSpan {
            from_ms,
            to_ms,
            cause,
            wake: timeline.wake_reason.clone(),
        }),
        _ => None,
    };
    Awake {
        boot_ms: timeline.boot_ms,
        last_sleep,
    }
}

/// The machine's boot and last sleep, read now (`kern.boottime`, `kern.sleeptime`,
/// `kern.waketime`, `kern.wakereason` and the root domain's `Last Sleep Reason`); `None` off
/// macOS or where the kernel's times are unreadable.
#[must_use]
pub fn awake() -> Option<Awake> {
    #[cfg(target_os = "macos")]
    {
        let timeline = crate::timeline::timeline()?;
        Some(awake_of(&timeline, crate::situation_macos::sleep_reason()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// What the [`Watch`] has heard, at one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Heard {
    /// The lid; `None` until read (or off macOS).
    pub lid: Option<LidState>,
    /// External displays attached; `None` until the display pipes were listed (or where no
    /// reader runs, [`DISPLAY_READER`]).
    pub external_displays: Option<u16>,
    /// The boot and last sleep; `None` until read.
    pub awake: Option<Awake>,
}

/// What the watch thread and its readers share.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    pub(crate) state: Mutex<Held>,
}

/// The state behind [`Heard`].
#[derive(Debug, Default)]
pub(crate) struct Held {
    pub(crate) lid: LidLatch,
    /// Registry entry ids of the external display pipes attached now; `None` until listed.
    pub(crate) external: Option<std::collections::BTreeSet<u64>>,
    pub(crate) awake: Option<Awake>,
}

/// The lid, the external displays and the last sleep, kept current by notifications on a
/// thread of the watch's own (macOS) until it is dropped; elsewhere it hears nothing.
#[derive(Debug)]
pub struct Watch {
    shared: Arc<Shared>,
    #[cfg(target_os = "macos")]
    _thread: Option<crate::situation_macos::Stop>,
}

/// A handle on a [`Watch`]'s state for another thread: the host's sleep listener tells it each
/// wake ([`Self::woke`]).
#[derive(Clone, Debug)]
pub struct WatchHandle(
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "off macOS a wake reads nothing again")
    )]
    Arc<Shared>,
);

impl Watch {
    /// Starts listening, then lists the display pipes and reads the lid and the last sleep, so
    /// nothing that moves between the two is missed (macOS); elsewhere a watch that hears
    /// nothing.
    #[must_use]
    pub fn start() -> Self {
        let shared = Arc::new(Shared::default());
        #[cfg(target_os = "macos")]
        {
            let thread = crate::situation_macos::listen(Arc::clone(&shared));
            if thread.is_none() {
                WatchHandle(Arc::clone(&shared)).woke();
            }
            Self {
                shared,
                _thread: thread,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self { shared }
        }
    }

    /// What it has heard.
    #[must_use]
    pub fn heard(&self) -> Heard {
        let held = self
            .shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Heard {
            lid: held.lid.state(),
            external_displays: held
                .external
                .as_ref()
                .map(|set| u16::try_from(set.len()).unwrap_or(u16::MAX)),
            awake: held.awake.clone(),
        }
    }

    /// A handle for another thread.
    #[must_use]
    pub fn handle(&self) -> WatchHandle {
        WatchHandle(Arc::clone(&self.shared))
    }
}

impl WatchHandle {
    /// The machine woke (or the watch starts): read the lid and the last sleep again. A lid
    /// that moved while nothing ran is a change at the moment this read it.
    pub fn woke(&self) {
        #[cfg(target_os = "macos")]
        crate::situation_macos::reread(
            &mut self.0.state.lock().unwrap_or_else(PoisonError::into_inner),
        );
    }
}

#[cfg(test)]
#[path = "situation_tests.rs"]
mod tests;
