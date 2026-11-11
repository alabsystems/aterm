// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Metal drawable present-to-glass latency and skip attribution.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

/// One presented handler's reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlassSample {
    /// The frame reached the display `ns` after `presentDrawable:` was registered.
    OnGlass {
        /// Registration → `presentedTime`, nanoseconds.
        ns: u64,
    },
    /// `presentedTime` was 0: the drawable was never shown.
    Skipped,
}

/// Pacing state a present was issued under, decided at handler registration from [`arm_present`].
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentTag {
    /// Registered at least one refresh period after the previous registration while visible.
    Spaced = 0,
    /// Registered within one refresh period while the keystroke pacing bypass (`input_hot`) was live.
    HotBurst = 1,
    /// Registered within one refresh period with no bypass live.
    IdleBurst = 2,
    /// Issued while the window was occlusion-hinted.
    Occluded = 3,
}

impl PresentTag {
    /// Number of tags.
    pub const COUNT: usize = 4;
    /// Every tag in [`Self::index`] order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Spaced,
        Self::HotBurst,
        Self::IdleBurst,
        Self::Occluded,
    ];

    /// Classify a present from its pacing state; occlusion takes precedence.
    #[must_use]
    #[inline]
    pub const fn for_pacing(burst: bool, input_hot: bool, occluded: bool) -> Self {
        match (occluded, burst, input_hot) {
            (true, _, _) => Self::Occluded,
            (false, true, true) => Self::HotBurst,
            (false, true, false) => Self::IdleBurst,
            (false, false, _) => Self::Spaced,
        }
    }

    /// Dense slot index in a per-tag array.
    #[must_use]
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Stable wire label.
    #[must_use]
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spaced => "spaced",
            Self::HotBurst => "hot_burst",
            Self::IdleBurst => "idle_burst",
            Self::Occluded => "occluded",
        }
    }

    #[must_use]
    #[inline]
    #[cfg(any(target_os = "macos", test))]
    const fn from_u8(raw: u8) -> Self {
        match raw {
            1 => Self::HotBurst,
            2 => Self::IdleBurst,
            3 => Self::Occluded,
            _ => Self::Spaced,
        }
    }
}

/// One presented handler's sample and attribution tags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlassReport {
    /// On glass or skipped.
    pub sample: GlassSample,
    /// Tag this present registered under.
    pub tag: PresentTag,
    /// Tag of the next serial's registration, or `tag` when none had registered yet.
    pub successor: PresentTag,
    /// Where this present showed a stats deck frame ([`arm_deck_target`]) and reached the
    /// display: how long after the vsync its deck clock targeted, ns (0 when on time).
    pub deck_late_ns: Option<u64>,
}

impl GlassReport {
    /// Attribute a skipped present to `Occluded`, the burst lane that replaced it, or `Spaced`.
    #[must_use]
    #[inline]
    pub const fn skip_attribution(self) -> PresentTag {
        match (self.tag, self.successor) {
            (PresentTag::Occluded, _) => PresentTag::Occluded,
            (_, PresentTag::HotBurst) => PresentTag::HotBurst,
            (_, PresentTag::IdleBurst) => PresentTag::IdleBurst,
            (_, PresentTag::Spaced | PresentTag::Occluded) => PresentTag::Spaced,
        }
    }
}

/// Pacing facts armed by the presenting thread before [`registration_begin`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArmedPresent {
    /// Keystroke pacing bypass is live in this window.
    pub input_hot: bool,
    /// Window is occlusion-hinted.
    pub occluded: bool,
    /// Window refresh period in nanoseconds.
    pub interval_ns: u64,
    /// Window's previous [`Registration::registered_ns`], or `None` before its first.
    pub prev_registered_ns: Option<u64>,
}

/// Registration record returned by [`take_registration`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registration {
    /// Monotonic serial starting at 1.
    pub serial: u64,
    /// `presentDrawable:` registration timestamp (`CACurrentMediaTime` nanoseconds).
    pub registered_ns: u64,
}

/// Frontend callback invoked on a framework thread; must not block.
pub type GlassSink = fn(GlassReport);

const ARM_HOT: u8 = 1;
const ARM_OCCLUDED: u8 = 2;
const ARM_HAS_PREV: u8 = 4;

#[cfg(any(target_os = "macos", test))]
const TAG_RING: usize = 16;

std::thread_local! {
    /// This thread's next registration's deck target ([`arm_deck_target`]),
    /// `CACurrentMediaTime` ns; 0 none. One per thread: the thread that arms a present is the one
    /// whose present registers it (the app's redraw, through the swapchain's present), so a
    /// present on another thread (a test's swapchain beside it) never takes it.
    static ARM_DECK_NS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The bits of a [`GlassRing::deck_targets`] slot that hold the target, ns: the rest hold the low
/// bits of the serial that wrote it. 2^56 ns is about 2.3 years of `CACurrentMediaTime`.
#[cfg(any(target_os = "macos", test))]
const DECK_TARGET_MASK: u64 = (1 << 56) - 1;

/// One present-to-glass domain: the process's is [`GLASS`]; a test builds its own.
#[derive(Debug)]
struct GlassRing {
    sink: OnceLock<GlassSink>,
    delivered: AtomicU64,
    arm_flags: AtomicU8,
    arm_interval_ns: AtomicU64,
    arm_prev_ns: AtomicU64,
    #[cfg(any(target_os = "macos", test))]
    next_serial: AtomicU64,
    takeable_serial: AtomicU64,
    takeable_ns: AtomicU64,
    /// Serial-tagged ring: each slot packs `(serial << 8) | tag` (Release/Acquire) so an
    /// overwritten slot never impersonates an earlier serial's successor.
    #[cfg(any(target_os = "macos", test))]
    tags: [AtomicU64; TAG_RING],
    /// Serial-indexed deck targets ([`arm_deck_target`]): each slot packs the low 8 bits of the
    /// serial that wrote it above its target, `CACurrentMediaTime` ns ([`DECK_TARGET_MASK`]; 0:
    /// the present showed no deck frame), so a slot a later registration overwrote reads as none
    /// rather than as that registration's target.
    #[cfg(any(target_os = "macos", test))]
    deck_targets: [AtomicU64; TAG_RING],
}

static GLASS: GlassRing = GlassRing::new();

impl GlassRing {
    const fn new() -> Self {
        Self {
            sink: OnceLock::new(),
            delivered: AtomicU64::new(0),
            arm_flags: AtomicU8::new(0),
            arm_interval_ns: AtomicU64::new(0),
            arm_prev_ns: AtomicU64::new(0),
            #[cfg(any(target_os = "macos", test))]
            next_serial: AtomicU64::new(1),
            takeable_serial: AtomicU64::new(0),
            takeable_ns: AtomicU64::new(0),
            #[cfg(any(target_os = "macos", test))]
            tags: [const { AtomicU64::new(0) }; TAG_RING],
            #[cfg(any(target_os = "macos", test))]
            deck_targets: [const { AtomicU64::new(0) }; TAG_RING],
        }
    }

    /// A test's own ring with `sink` installed, so no other test's present reaches it.
    #[cfg(test)]
    fn listening(sink: GlassSink) -> Self {
        let ring = Self::new();
        assert!(ring.install_sink(sink));
        ring
    }

    fn install_sink(&self, sink: GlassSink) -> bool {
        self.sink.set(sink).is_ok()
    }

    #[inline]
    #[cfg(target_os = "macos")]
    fn sink_installed(&self) -> bool {
        self.sink.get().is_some()
    }

    #[inline]
    fn delivered(&self) -> u64 {
        self.delivered.load(Ordering::Relaxed)
    }

    #[inline]
    fn arm_present(&self, armed: ArmedPresent) {
        let mut flags = 0;
        if armed.input_hot {
            flags |= ARM_HOT;
        }
        if armed.occluded {
            flags |= ARM_OCCLUDED;
        }
        if let Some(prev) = armed.prev_registered_ns {
            flags |= ARM_HAS_PREV;
            self.arm_prev_ns.store(prev, Ordering::Relaxed);
        }
        self.arm_interval_ns
            .store(armed.interval_ns, Ordering::Relaxed);
        self.arm_flags.store(flags, Ordering::Relaxed);
    }

    #[cfg(any(target_os = "macos", test))]
    fn registration_begin(&self, registered_s: f64) -> (u64, PresentTag) {
        let registered_ns = secs_to_ns(registered_s);
        let flags = self.arm_flags.load(Ordering::Relaxed);
        let burst = flags & ARM_HAS_PREV != 0 && {
            let prev = self.arm_prev_ns.load(Ordering::Relaxed);
            registered_ns.saturating_sub(prev) < self.arm_interval_ns.load(Ordering::Relaxed)
        };
        let tag = PresentTag::for_pacing(burst, flags & ARM_HOT != 0, flags & ARM_OCCLUDED != 0);
        let serial = self.next_serial.fetch_add(1, Ordering::Relaxed);
        let target = ARM_DECK_NS
            .with(|armed| armed.replace(0))
            .min(DECK_TARGET_MASK);
        self.deck_targets[ring_slot(serial)]
            .store(((serial & 0xff) << 56) | target, Ordering::Relaxed);
        let entry = (serial << 8) | u64::from(tag as u8);
        self.tags[ring_slot(serial)].store(entry, Ordering::Release);
        self.takeable_ns.store(registered_ns, Ordering::Relaxed);
        self.takeable_serial.store(serial, Ordering::Relaxed);
        (serial, tag)
    }

    #[inline]
    fn take_registration(&self) -> Option<Registration> {
        match self.takeable_serial.swap(0, Ordering::Relaxed) {
            0 => None,
            serial => Some(Registration {
                serial,
                registered_ns: self.takeable_ns.load(Ordering::Relaxed),
            }),
        }
    }

    /// The deck target registration `serial` armed ([`arm_deck_target`]), ns; 0 for none, or
    /// for a slot a later registration has overwritten since.
    #[cfg(any(target_os = "macos", test))]
    fn deck_target(&self, serial: u64) -> u64 {
        let entry = self.deck_targets[ring_slot(serial)].load(Ordering::Relaxed);
        if entry >> 56 == serial & 0xff {
            entry & DECK_TARGET_MASK
        } else {
            0
        }
    }

    /// Deliver the presented handler of registration `serial` at `registered_s` that reached
    /// the display at `presented_s` (`CACurrentMediaTime` seconds; 0 never shown): its sample,
    /// and its deck frame's lateness against the vsync its tick targeted ([`arm_deck_target`]).
    #[cfg(any(target_os = "macos", test))]
    fn deliver_presented(&self, serial: u64, tag: PresentTag, registered_s: f64, presented_s: f64) {
        let sample = classify(registered_s, presented_s);
        let late = deck_late(self.deck_target(serial), sample, presented_s);
        self.deliver(serial, tag, sample, late);
    }

    /// Deliver one handler's sample to the sink with the next serial's tag as successor.
    #[cfg(test)]
    fn deliver_report(&self, serial: u64, tag: PresentTag, sample: GlassSample) {
        self.deliver(serial, tag, sample, None);
    }

    #[cfg(any(target_os = "macos", test))]
    fn deliver(
        &self,
        serial: u64,
        tag: PresentTag,
        sample: GlassSample,
        deck_late_ns: Option<u64>,
    ) {
        self.delivered.fetch_add(1, Ordering::Relaxed);
        let successor = match serial.checked_add(1) {
            Some(next) if (next >> 56) == 0 => {
                let entry = self.tags[ring_slot(next)].load(Ordering::Acquire);
                if (entry >> 8) == next {
                    PresentTag::from_u8(entry as u8)
                } else {
                    tag
                }
            }
            _ => tag,
        };
        if let Some(sink) = self.sink.get() {
            sink(GlassReport {
                sample,
                tag,
                successor,
                deck_late_ns,
            });
        }
    }
}

/// Install the process sink; returns `false` if already installed.
pub fn install_sink(sink: GlassSink) -> bool {
    GLASS.install_sink(sink)
}

#[inline]
#[cfg(target_os = "macos")]
pub(crate) fn sink_installed() -> bool {
    GLASS.sink_installed()
}

/// Total presented handlers delivered since process start.
#[must_use]
#[inline]
pub fn delivered() -> u64 {
    GLASS.delivered()
}

/// Arm the pacing facts for the next present on the presenting thread.
#[inline]
pub fn arm_present(armed: ArmedPresent) {
    GLASS.arm_present(armed);
}

/// Arm this thread's next present as one that shows a stats deck frame whose clock tick
/// targeted the vsync at `target_ns` (`CACurrentMediaTime` ns, [`media_ns_of`]), or as one that
/// shows none. Its report then carries [`GlassReport::deck_late_ns`].
#[inline]
pub fn arm_deck_target(target_ns: Option<u64>) {
    ARM_DECK_NS.with(|armed| armed.set(target_ns.unwrap_or(0)));
}

/// `at` on the clock a drawable's `presentedTime` reads (`CACurrentMediaTime`), ns, through one
/// (`CACurrentMediaTime`, `Instant`) pair; `None` off macOS.
#[must_use]
pub fn media_ns_of(at: std::time::Instant) -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        static PAIR: OnceLock<(f64, std::time::Instant)> = OnceLock::new();
        let &(media_s, now) = PAIR.get_or_init(|| {
            // SAFETY: reading a monotonic clock.
            (
                unsafe { crate::metal::swapchain::media_time_s() },
                std::time::Instant::now(),
            )
        });
        let secs = if at >= now {
            media_s + (at - now).as_secs_f64()
        } else {
            media_s - (now - at).as_secs_f64()
        };
        Some(secs_to_ns(secs))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = at;
        None
    }
}

/// `secs` as whole ns: 0 for a span that is not positive or not finite, `u64::MAX` past the
/// range.
#[inline]
fn secs_to_ns(secs: f64) -> u64 {
    if secs.is_finite() && secs > 0.0 {
        // A value past `Duration`'s range saturates: this runs in a presented handler, where a
        // panic would cross the Objective-C boundary.
        Duration::try_from_secs_f64(secs).map_or(u64::MAX, |d| {
            u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
        })
    } else {
        0
    }
}

/// Record a registration at `registered_s` (`CACurrentMediaTime` seconds), publish its tag,
/// and leave it takeable.
#[cfg(target_os = "macos")]
pub(crate) fn registration_begin(registered_s: f64) -> (u64, PresentTag) {
    GLASS.registration_begin(registered_s)
}

/// Take the most recent registration once; returns `None` if nothing registered since the last take.
#[inline]
pub fn take_registration() -> Option<Registration> {
    GLASS.take_registration()
}

#[inline]
#[cfg(any(target_os = "macos", test))]
fn ring_slot(serial: u64) -> usize {
    usize::try_from(serial % TAG_RING as u64).unwrap_or(0)
}

/// Deliver the presented handler of registration `serial` at `registered_s` that reached the
/// display at `presented_s` (`CACurrentMediaTime` seconds; 0 never shown): its sample, and its
/// deck frame's lateness against the vsync its tick targeted ([`arm_deck_target`]).
#[cfg(target_os = "macos")]
pub(crate) fn deliver_presented(serial: u64, tag: PresentTag, registered_s: f64, presented_s: f64) {
    GLASS.deliver_presented(serial, tag, registered_s, presented_s);
}

/// A deck frame's lateness: how long after its tick's vsync (`target_ns`, 0 for a present that
/// showed no deck frame) it reached the display at `presented_s`, 0 when on time; `None` for no
/// deck frame or one never shown.
#[cfg(any(target_os = "macos", test))]
fn deck_late(target_ns: u64, sample: GlassSample, presented_s: f64) -> Option<u64> {
    match sample {
        GlassSample::OnGlass { .. } if target_ns != 0 => {
            Some(secs_to_ns(presented_s).saturating_sub(target_ns))
        }
        _ => None,
    }
}

/// Classify a presented handler's `registered_s` and `presented_s` (`CACurrentMediaTime` seconds).
#[must_use]
#[inline]
pub fn classify(registered_s: f64, presented_s: f64) -> GlassSample {
    if presented_s.is_nan() || presented_s <= 0.0 {
        return GlassSample::Skipped;
    }
    GlassSample::OnGlass {
        ns: secs_to_ns(presented_s - registered_s),
    }
}

#[cfg(test)]
#[path = "present_glass_tests.rs"]
mod tests;
