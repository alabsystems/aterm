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

static SINK: OnceLock<GlassSink> = OnceLock::new();
static DELIVERED: AtomicU64 = AtomicU64::new(0);

const ARM_HOT: u8 = 1;
const ARM_OCCLUDED: u8 = 2;
const ARM_HAS_PREV: u8 = 4;
static ARM_FLAGS: AtomicU8 = AtomicU8::new(0);
static ARM_INTERVAL_NS: AtomicU64 = AtomicU64::new(0);
static ARM_PREV_NS: AtomicU64 = AtomicU64::new(0);

#[cfg(any(target_os = "macos", test))]
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);
static TAKEABLE_SERIAL: AtomicU64 = AtomicU64::new(0);
static TAKEABLE_NS: AtomicU64 = AtomicU64::new(0);

/// Serial-indexed tag ring published via `REGISTERED` (Release/Acquire).
#[cfg(any(target_os = "macos", test))]
const TAG_RING: usize = 16;
#[cfg(any(target_os = "macos", test))]
static TAGS: [AtomicU8; TAG_RING] = [const { AtomicU8::new(PresentTag::Spaced as u8) }; TAG_RING];
#[cfg(any(target_os = "macos", test))]
static REGISTERED: AtomicU64 = AtomicU64::new(0);

/// Install the process sink; returns `false` if already installed.
pub fn install_sink(sink: GlassSink) -> bool {
    SINK.set(sink).is_ok()
}

#[inline]
#[cfg(target_os = "macos")]
pub(crate) fn sink_installed() -> bool {
    SINK.get().is_some()
}

/// Total presented handlers delivered since process start.
#[must_use]
#[inline]
pub fn delivered() -> u64 {
    DELIVERED.load(Ordering::Relaxed)
}

/// Arm the pacing facts for the next present on the presenting thread.
#[inline]
pub fn arm_present(armed: ArmedPresent) {
    let mut flags = 0;
    if armed.input_hot {
        flags |= ARM_HOT;
    }
    if armed.occluded {
        flags |= ARM_OCCLUDED;
    }
    if let Some(prev) = armed.prev_registered_ns {
        flags |= ARM_HAS_PREV;
        ARM_PREV_NS.store(prev, Ordering::Relaxed);
    }
    ARM_INTERVAL_NS.store(armed.interval_ns, Ordering::Relaxed);
    ARM_FLAGS.store(flags, Ordering::Relaxed);
}

#[inline]
#[cfg(any(target_os = "macos", test))]
fn secs_to_ns(secs: f64) -> u64 {
    if secs.is_finite() && secs > 0.0 {
        u64::try_from(Duration::from_secs_f64(secs).as_nanos()).unwrap_or(u64::MAX)
    } else {
        0
    }
}

/// Record a registration at `registered_s` (`CACurrentMediaTime` seconds), publish its tag,
/// and leave it takeable.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn registration_begin(registered_s: f64) -> (u64, PresentTag) {
    let registered_ns = secs_to_ns(registered_s);
    let flags = ARM_FLAGS.load(Ordering::Relaxed);
    let burst = flags & ARM_HAS_PREV != 0 && {
        let prev = ARM_PREV_NS.load(Ordering::Relaxed);
        registered_ns.saturating_sub(prev) < ARM_INTERVAL_NS.load(Ordering::Relaxed)
    };
    let tag = PresentTag::for_pacing(burst, flags & ARM_HOT != 0, flags & ARM_OCCLUDED != 0);
    let serial = NEXT_SERIAL.fetch_add(1, Ordering::Relaxed);
    TAGS[ring_slot(serial)].store(tag as u8, Ordering::Relaxed);
    REGISTERED.store(serial, Ordering::Release);
    TAKEABLE_NS.store(registered_ns, Ordering::Relaxed);
    TAKEABLE_SERIAL.store(serial, Ordering::Relaxed);
    (serial, tag)
}

/// Take the most recent registration once; returns `None` if nothing registered since the last take.
#[inline]
pub fn take_registration() -> Option<Registration> {
    match TAKEABLE_SERIAL.swap(0, Ordering::Relaxed) {
        0 => None,
        serial => Some(Registration {
            serial,
            registered_ns: TAKEABLE_NS.load(Ordering::Relaxed),
        }),
    }
}

#[inline]
#[cfg(any(target_os = "macos", test))]
fn ring_slot(serial: u64) -> usize {
    usize::try_from(serial % TAG_RING as u64).unwrap_or(0)
}

/// Deliver one handler's sample to the sink with the next serial's tag as successor.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn deliver_report(serial: u64, tag: PresentTag, sample: GlassSample) {
    DELIVERED.fetch_add(1, Ordering::Relaxed);
    let successor = if REGISTERED.load(Ordering::Acquire) > serial {
        PresentTag::from_u8(TAGS[ring_slot(serial.wrapping_add(1))].load(Ordering::Relaxed))
    } else {
        tag
    };
    if let Some(sink) = SINK.get() {
        sink(GlassReport {
            sample,
            tag,
            successor,
        });
    }
}

/// Classify a presented handler's `registered_s` and `presented_s` (`CACurrentMediaTime` seconds).
#[must_use]
#[inline]
pub fn classify(registered_s: f64, presented_s: f64) -> GlassSample {
    if presented_s.is_nan() || presented_s <= 0.0 {
        return GlassSample::Skipped;
    }
    let secs = presented_s - registered_s;
    let ns = if secs.is_finite() && secs > 0.0 {
        Duration::from_secs_f64(secs).as_nanos()
    } else {
        0
    };
    GlassSample::OnGlass {
        ns: u64::try_from(ns).unwrap_or(u64::MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ArmedPresent, GlassReport, GlassSample, PresentTag, arm_present, classify, deliver_report,
        delivered, registration_begin, take_registration,
    };
    use std::sync::Mutex;

    static SERIAL: Mutex<()> = Mutex::new(());

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
        let _turn = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let hz60_ns = 1_000_000_000_000 / 60_000;
        arm_present(ArmedPresent {
            input_hot: true,
            occluded: false,
            interval_ns: hz60_ns,
            prev_registered_ns: None,
        });
        let (first, tag) = registration_begin(100.0);
        assert_eq!(
            tag,
            PresentTag::Spaced,
            "no previous registration is not a burst"
        );
        let reg = take_registration().expect("registration is takeable");
        assert_eq!(reg.serial, first);
        assert_eq!(reg.registered_ns, 100_000_000_000);
        assert_eq!(take_registration(), None, "taken once");

        arm_present(ArmedPresent {
            input_hot: true,
            occluded: false,
            interval_ns: hz60_ns,
            prev_registered_ns: Some(reg.registered_ns),
        });
        let (_, tag) = registration_begin(100.008_33);
        assert_eq!(tag, PresentTag::HotBurst);

        arm_present(ArmedPresent {
            input_hot: false,
            occluded: false,
            interval_ns: hz60_ns,
            prev_registered_ns: Some(reg.registered_ns),
        });
        let (_, tag) = registration_begin(100.016_67);
        assert_eq!(tag, PresentTag::Spaced);

        arm_present(ArmedPresent {
            input_hot: false,
            occluded: false,
            interval_ns: hz60_ns,
            prev_registered_ns: Some(reg.registered_ns),
        });
        let (_, tag) = registration_begin(100.004);
        assert_eq!(tag, PresentTag::IdleBurst);
        let _ = take_registration();
    }

    #[test]
    fn a_report_names_the_next_registration_as_its_successor() {
        let _turn = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let spaced = ArmedPresent {
            input_hot: false,
            occluded: false,
            interval_ns: 16_666_666,
            prev_registered_ns: None,
        };
        arm_present(spaced);
        let (a, tag_a) = registration_begin(200.0);
        arm_present(ArmedPresent {
            input_hot: true,
            prev_registered_ns: Some(200_000_000_000),
            ..spaced
        });
        let (b, tag_b) = registration_begin(200.005);
        assert_eq!(tag_b, PresentTag::HotBurst);
        arm_present(spaced);
        let (c, tag_c) = registration_begin(201.0);
        assert_eq!(tag_c, PresentTag::Spaced);
        let _ = take_registration();

        static SEEN: Mutex<Vec<GlassReport>> = Mutex::new(Vec::new());
        fn sink(report: GlassReport) {
            SEEN.lock().unwrap().push(report);
        }
        let ours = super::install_sink(sink);
        let before = delivered();
        deliver_report(a, tag_a, GlassSample::Skipped);
        deliver_report(c, tag_c, GlassSample::OnGlass { ns: 1 });
        deliver_report(b, tag_b, GlassSample::OnGlass { ns: 1 });
        assert_eq!(delivered(), before + 3);
        if ours {
            let seen = SEEN.lock().unwrap();
            assert_eq!(seen[0].successor, PresentTag::HotBurst, "a → b");
            assert_eq!(seen[0].skip_attribution(), PresentTag::HotBurst);
            assert_eq!(seen[1].successor, tag_c, "c has nothing after it");
        }
    }
}
