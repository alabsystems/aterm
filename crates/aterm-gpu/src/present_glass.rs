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

    #[cfg(any(target_os = "macos", test))]
    fn deliver_report(&self, serial: u64, tag: PresentTag, sample: GlassSample) {
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

#[inline]
fn secs_to_ns(secs: f64) -> u64 {
    if secs.is_finite() && secs > 0.0 {
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

/// Deliver one handler's sample to the sink with the next serial's tag as successor.
#[cfg(target_os = "macos")]
pub(crate) fn deliver_report(serial: u64, tag: PresentTag, sample: GlassSample) {
    GLASS.deliver_report(serial, tag, sample);
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
mod tests {
    use super::{
        ArmedPresent, GlassReport, GlassRing, GlassSample, PresentTag, TAG_RING, classify,
    };
    use std::sync::Mutex;

    #[test]
    fn classify_skips_unpresented_frames_and_clamps_the_span() {
        assert_eq!(classify(12.5, 0.0), GlassSample::Skipped);
        assert_eq!(classify(12.5, f64::NAN), GlassSample::Skipped);
        assert_eq!(classify(12.5, -1.0), GlassSample::Skipped);
        let GlassSample::OnGlass { ns } = classify(100.0, 100.016) else {
            panic!("a nonzero presentedTime is on glass");
        };
        assert!(
            (15_999_000..=16_001_000).contains(&ns),
            "16 ms span read as {ns} ns"
        );
        assert_eq!(classify(100.0, 99.0), GlassSample::OnGlass { ns: 0 });
        assert_eq!(
            classify(0.0, 1.0e30),
            GlassSample::OnGlass { ns: u64::MAX },
            "spans exceeding Duration::MAX saturate to u64::MAX instead of panicking"
        );
        assert_eq!(
            classify(-1.0e308, 1.0e308),
            GlassSample::OnGlass { ns: 0 },
            "infinite spans fail closed to 0"
        );
    }

    #[test]
    fn occlusion_wins_then_burst_splits_on_the_bypass() {
        assert_eq!(
            PresentTag::for_pacing(true, true, true),
            PresentTag::Occluded
        );
        assert_eq!(
            PresentTag::for_pacing(false, false, true),
            PresentTag::Occluded
        );
        assert_eq!(
            PresentTag::for_pacing(true, true, false),
            PresentTag::HotBurst
        );
        assert_eq!(
            PresentTag::for_pacing(true, false, false),
            PresentTag::IdleBurst
        );
        assert_eq!(
            PresentTag::for_pacing(false, true, false),
            PresentTag::Spaced
        );
        assert_eq!(
            PresentTag::for_pacing(false, false, false),
            PresentTag::Spaced
        );
    }

    #[test]
    fn a_skip_is_attributed_to_its_successor_unless_occluded() {
        let report = |tag, successor| GlassReport {
            sample: GlassSample::Skipped,
            tag,
            successor,
        };
        assert_eq!(
            report(PresentTag::Spaced, PresentTag::HotBurst).skip_attribution(),
            PresentTag::HotBurst
        );
        assert_eq!(
            report(PresentTag::HotBurst, PresentTag::IdleBurst).skip_attribution(),
            PresentTag::IdleBurst
        );
        assert_eq!(
            report(PresentTag::Occluded, PresentTag::HotBurst).skip_attribution(),
            PresentTag::Occluded
        );
        assert_eq!(
            report(PresentTag::HotBurst, PresentTag::Spaced).skip_attribution(),
            PresentTag::Spaced
        );
        assert_eq!(
            report(PresentTag::Spaced, PresentTag::Occluded).skip_attribution(),
            PresentTag::Spaced
        );
    }

    #[test]
    fn the_tag_is_decided_at_registration_against_the_previous_registration() {
        let ring = GlassRing::new();
        let hz60_ns = 1_000_000_000_000 / 60_000;
        ring.arm_present(ArmedPresent {
            input_hot: true,
            occluded: false,
            interval_ns: hz60_ns,
            prev_registered_ns: None,
        });
        let (first, tag) = ring.registration_begin(100.0);
        assert_eq!(
            tag,
            PresentTag::Spaced,
            "no previous registration is not a burst"
        );
        let reg = ring.take_registration().expect("registration is takeable");
        assert_eq!(reg.serial, first);
        assert_eq!(reg.registered_ns, 100_000_000_000);
        assert_eq!(ring.take_registration(), None, "taken once");

        ring.arm_present(ArmedPresent {
            input_hot: true,
            occluded: false,
            interval_ns: hz60_ns,
            prev_registered_ns: Some(reg.registered_ns),
        });
        let (_, tag) = ring.registration_begin(100.008_33);
        assert_eq!(tag, PresentTag::HotBurst);

        ring.arm_present(ArmedPresent {
            input_hot: false,
            occluded: false,
            interval_ns: hz60_ns,
            prev_registered_ns: Some(reg.registered_ns),
        });
        let (_, tag) = ring.registration_begin(100.016_67);
        assert_eq!(tag, PresentTag::Spaced);

        ring.arm_present(ArmedPresent {
            input_hot: false,
            occluded: false,
            interval_ns: hz60_ns,
            prev_registered_ns: Some(reg.registered_ns),
        });
        let (_, tag) = ring.registration_begin(100.004);
        assert_eq!(tag, PresentTag::IdleBurst);
    }

    #[test]
    fn a_report_names_the_next_registration_as_its_successor() {
        static SEEN: Mutex<Vec<GlassReport>> = Mutex::new(Vec::new());
        fn sink(report: GlassReport) {
            SEEN.lock().unwrap().push(report);
        }
        let ring = GlassRing::listening(sink);
        let spaced = ArmedPresent {
            input_hot: false,
            occluded: false,
            interval_ns: 16_666_666,
            prev_registered_ns: None,
        };
        ring.arm_present(spaced);
        let (a, tag_a) = ring.registration_begin(200.0);
        ring.arm_present(ArmedPresent {
            input_hot: true,
            prev_registered_ns: Some(200_000_000_000),
            ..spaced
        });
        let (b, tag_b) = ring.registration_begin(200.005);
        assert_eq!(tag_b, PresentTag::HotBurst);
        ring.arm_present(spaced);
        let (c, tag_c) = ring.registration_begin(201.0);
        assert_eq!(tag_c, PresentTag::Spaced);

        ring.deliver_report(a, tag_a, GlassSample::Skipped);
        ring.deliver_report(c, tag_c, GlassSample::OnGlass { ns: 1 });
        ring.deliver_report(b, tag_b, GlassSample::OnGlass { ns: 1 });
        assert_eq!(ring.delivered(), 3);
        {
            let seen = SEEN.lock().unwrap();
            assert_eq!(seen.len(), 3);
            assert_eq!(seen[0].successor, PresentTag::HotBurst, "a → b");
            assert_eq!(seen[0].skip_attribution(), PresentTag::HotBurst);
            assert_eq!(seen[1].successor, tag_c, "c has nothing after it");
        }

        ring.arm_present(spaced);
        let (d, tag_d) = ring.registration_begin(202.0);
        assert_eq!(tag_d, PresentTag::Spaced);
        for i in 0..=u16::try_from(TAG_RING).unwrap() {
            ring.arm_present(ArmedPresent {
                input_hot: true,
                prev_registered_ns: Some(202_000_000_000),
                ..spaced
            });
            let (_, later_tag) = ring.registration_begin(202.001 + f64::from(i) * 0.000_1);
            assert_eq!(later_tag, PresentTag::HotBurst);
        }
        ring.deliver_report(d, tag_d, GlassSample::Skipped);
        let seen = SEEN.lock().unwrap();
        assert_eq!(
            seen[3].successor, tag_d,
            "an overwritten TAG_RING slot falls back to d's own tag instead of impersonating d + 1"
        );
        assert_eq!(seen[3].skip_attribution(), PresentTag::Spaced);
    }
}
