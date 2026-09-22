// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PRESENT → GLASS: the span after application present-return that the
//! frontend's `metrics` ledger could not see.
//!
//! `aterm-gui`'s `present_latency` and `input_present` both stop at application
//! present-return: `Frame::present` registers `presentDrawable:`, commits, and
//! returns. Whether a slow reading was the window server holding the drawable
//! or a parked main thread was structurally unanswerable — the 2026-09 audit's
//! 560 ms `max_present_latency_ms` beside `present_drops=0` could not be
//! attributed to either.
//!
//! On the shipped macOS Metal path, `Frame::present` hangs an
//! `addPresentedHandler:` on the drawable whenever a sink is installed. The
//! handler reads `-[MTLDrawable presentedTime]` — the host time the frame
//! reached the display, on CoreAnimation's `CACurrentMediaTime` clock — and
//! reports the interval from the `presentDrawable:` registration (read on the
//! same clock immediately before it) to that instant. A drawable the compositor
//! skipped or never showed (an occluded window, a frame replaced before its
//! vsync, an unparented layer) reports `presentedTime == 0`, delivered as
//! [`GlassSample::Skipped`] rather than as a fabricated duration.
//!
//! # The second audit: WHICH skips
//!
//! The first handler answered "how long to glass"; it could not answer "why
//! was this one never shown". Measured on the owner's A18 Pro daily driver
//! (2026-09-21, 60 Hz panel, `displaySyncEnabled = NO`): 1,310 of 69,836
//! frames skipped over 16 h, and they tracked TYPING — 21 keystroke-free
//! minutes of streaming output (10,434 frames) produced 4, the next 224
//! keystrokes produced 119. The mechanism is the keystroke pacing bypass:
//! `input_hot` halves the content pace floor to `interval / 2` for the 50 ms
//! after a key, so a TUI repaint burst inside that window presents at 2× the
//! refresh rate, and with display sync off the compositor shows the newer
//! drawable and drops the older one — a full render, encode and present spent
//! on pixels no one saw.
//!
//! One addition makes that attributable without a second handler: **a tag per
//! present, decided at registration, on the registration clock.** The
//! presenting thread arms the pacing FACTS ([`arm_present`]: the bypass state,
//! the occlusion hint, the refresh period, and this window's previous
//! registration instant) immediately before the present that registers the
//! handler; [`registration_begin`] reads `CACurrentMediaTime` for the
//! registration, decides "within one refresh of the previous registration"
//! from it, and mints a serial. Deciding at registration rather than at arm
//! time matters exactly where skips happen: under GPU contention a present can
//! park for tens of milliseconds in `nextDrawable` between the two, and a tag
//! taken at arm time would then call a well-spaced present a burst.
//!
//! A skipped frame is charged to its SUCCESSOR — the registration with the
//! NEXT serial, looked up in a serial-indexed ring, so it is the same frame
//! whether the handler fires before or after further registrations. For a
//! single window that is the frame that replaced it. With several windows open
//! the next serial can be another window's present, which shares no layer and
//! replaced nothing; that attribution is then wrong by one lane, and the
//! process-wide instrument accepts it rather than carrying per-layer state
//! into a handler that must not allocate. A skip whose successor is `Spaced`
//! (or its own `Occluded` tag) was not a replacement: the compositor dropped
//! it, or nobody could have seen it.
//!
//! A pacing GATE on this signal — hold a content present until the previous
//! drawable settles, fail open at one refresh — was implemented and measured
//! on 2026-09-21 against this build over the control socket: no latency
//! regression in four paired runs, and no measurable gain either, because the
//! skip condition did not reproduce under synthetic load (spin hogs leave the
//! compositor responsive; glass p50 fell to 2.9 ms). It was dropped as
//! unproven complexity. The attribution here is what decides whether it is
//! ever worth re-adding: `present_glass_skipped_by=hot_burst` on a daily
//! driver under its real workload.
//!
//! This crate publishes nothing itself: the frontend installs one sink with
//! [`install_sink`] and folds the reports into its ledger. No sink, no handler —
//! a process that never installs one pays nothing per present.
//!
//! The handler runs on a Metal/CoreAnimation-owned thread, so a sink must be
//! lock-free and must not block or unwind (the frontend's is a histogram bucket
//! increment).

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

/// The pacing state a present was issued under, decided at handler
/// registration from the facts the presenting thread armed ([`arm_present`]).
/// The wire labels are stable: `aterm ctl metrics percentiles` prints them.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentTag {
    /// At least one refresh period after this window's previous registration,
    /// window visible. A skip attributed here was not a replacement.
    Spaced = 0,
    /// Within one refresh period of the previous registration while the
    /// keystroke pacing bypass (`input_hot`) was live — the 2× lane the audit
    /// measured.
    HotBurst = 1,
    /// Within one refresh period of the previous registration with no bypass
    /// live — two lanes colliding (an effect-lane frame beside a content frame).
    IdleBurst = 2,
    /// The window was occlusion-hinted when the present was issued: nobody could
    /// have seen it, whatever the compositor did.
    Occluded = 3,
}

impl PresentTag {
    /// How many tags there are — the ledger arrays are sized by it.
    pub const COUNT: usize = 4;
    /// Every tag, dense and in `index` order (pinned by a test).
    pub const ALL: [Self; Self::COUNT] = [
        Self::Spaced,
        Self::HotBurst,
        Self::IdleBurst,
        Self::Occluded,
    ];

    /// The tag for a present issued under this pacing state. Occlusion wins
    /// (nobody could see it); then whether it registered within one refresh of
    /// the previous registration, and under which pace floor.
    #[must_use]
    pub const fn for_pacing(burst: bool, input_hot: bool, occluded: bool) -> Self {
        match (occluded, burst, input_hot) {
            (true, _, _) => Self::Occluded,
            (false, true, true) => Self::HotBurst,
            (false, true, false) => Self::IdleBurst,
            (false, false, _) => Self::Spaced,
        }
    }

    /// The dense slot this tag owns in a per-tag array.
    #[must_use]
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The stable wire label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spaced => "spaced",
            Self::HotBurst => "hot_burst",
            Self::IdleBurst => "idle_burst",
            Self::Occluded => "occluded",
        }
    }

    /// The tag a stored discriminant names; anything else is `Spaced`, the
    /// arm that claims nothing about pacing.
    #[must_use]
    const fn from_u8(raw: u8) -> Self {
        match raw {
            1 => Self::HotBurst,
            2 => Self::IdleBurst,
            3 => Self::Occluded,
            _ => Self::Spaced,
        }
    }
}

/// One presented handler's report: the sample plus what the frontend needs to
/// attribute it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlassReport {
    /// On glass, or never shown.
    pub sample: GlassSample,
    /// The tag THIS present registered under.
    pub tag: PresentTag,
    /// The tag of the registration with the NEXT serial — for a single window,
    /// the frame that replaced this one. Equal to `tag` when nothing has
    /// registered after this present by the time its handler fired.
    pub successor: PresentTag,
}

impl GlassReport {
    /// Where a SKIP belongs: an occluded present is its own explanation; a
    /// visible one whose successor registered within a refresh is charged to the
    /// lane that replaced it; one nothing replaced was dropped by the compositor.
    #[must_use]
    pub fn skip_attribution(self) -> PresentTag {
        match (self.tag, self.successor) {
            (PresentTag::Occluded, _) => PresentTag::Occluded,
            (_, PresentTag::HotBurst) => PresentTag::HotBurst,
            (_, PresentTag::IdleBurst) => PresentTag::IdleBurst,
            (_, PresentTag::Spaced | PresentTag::Occluded) => PresentTag::Spaced,
        }
    }
}

/// The pacing facts the presenting thread arms for the present it is about to
/// issue ([`arm_present`]); [`registration_begin`] decides the tag from them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArmedPresent {
    /// The keystroke pacing bypass is live in this window.
    pub input_hot: bool,
    /// The window is occlusion-hinted.
    pub occluded: bool,
    /// This window's refresh period, nanoseconds.
    pub interval_ns: u64,
    /// This window's previous registration instant on the registration clock
    /// (`Registration::registered_ns`), or `None` before its first.
    pub prev_registered_ns: Option<u64>,
}

/// One registration, as the presenting thread takes it back
/// ([`take_registration`]) right after its present returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registration {
    /// Monotonic from 1.
    pub serial: u64,
    /// The `presentDrawable:` registration instant, `CACurrentMediaTime` in
    /// nanoseconds — the value to arm as the next present's `prev_registered_ns`.
    pub registered_ns: u64,
}

/// The frontend's receiver. Called on a framework thread; must not block.
pub type GlassSink = fn(GlassReport);

static SINK: OnceLock<GlassSink> = OnceLock::new();
static DELIVERED: AtomicU64 = AtomicU64::new(0);

// The armed facts: written and read on the presenting thread only (arm, then
// register, in that order on one thread), so `Relaxed` is the whole story.
const ARM_HOT: u8 = 1;
const ARM_OCCLUDED: u8 = 2;
const ARM_HAS_PREV: u8 = 4;
static ARM_FLAGS: AtomicU8 = AtomicU8::new(0);
static ARM_INTERVAL_NS: AtomicU64 = AtomicU64::new(0);
static ARM_PREV_NS: AtomicU64 = AtomicU64::new(0);

/// Registration serials start at 1; 0 means "none".
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);
/// The most recent registration, until the presenting thread takes it
/// (`take_registration`): serial (0 = none) and its registration instant.
static TAKEABLE_SERIAL: AtomicU64 = AtomicU64::new(0);
static TAKEABLE_NS: AtomicU64 = AtomicU64::new(0);

/// The tag ring: slot `serial % TAG_RING` holds that serial's tag, published to
/// the handler thread by `REGISTERED` (Release at registration, Acquire in the
/// handler): a handler that observes `REGISTERED >= s` observes `TAGS[s]`. A
/// layer holds at most three drawables and the frontend opens few windows, so a
/// slot is reused only after sixteen further registrations — long after any
/// handler that could ask about it has fired.
const TAG_RING: usize = 16;
static TAGS: [AtomicU8; TAG_RING] = [const { AtomicU8::new(PresentTag::Spaced as u8) }; TAG_RING];
/// The highest serial whose `TAGS` slot is written (0 = none).
static REGISTERED: AtomicU64 = AtomicU64::new(0);

/// Install the process's one sink. First install wins; `false` when a sink was
/// already installed.
pub fn install_sink(sink: GlassSink) -> bool {
    SINK.set(sink).is_ok()
}

/// Whether a sink is installed — the swapchain registers a handler only then.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the macOS Metal present registers handlers")
)]
pub(crate) fn sink_installed() -> bool {
    SINK.get().is_some()
}

/// Presented handlers delivered since process start, both kinds.
#[must_use]
pub fn delivered() -> u64 {
    DELIVERED.load(Ordering::Relaxed)
}

/// Arm the pacing facts of the present about to be issued. Called by the
/// presenting thread immediately before the present that registers the handler.
/// A present that never registers (refused, non-Metal path) leaves the facts
/// for the next one, which overwrites them.
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

/// `CACurrentMediaTime` seconds → nanoseconds, saturating.
fn secs_to_ns(secs: f64) -> u64 {
    if secs.is_finite() && secs > 0.0 {
        u64::try_from(Duration::from_secs_f64(secs).as_nanos()).unwrap_or(u64::MAX)
    } else {
        0
    }
}

/// Begin one registration at `registered_s` (`CACurrentMediaTime` seconds, read
/// by the caller immediately before `presentDrawable:`): mint its serial, decide
/// its tag from the armed facts, publish the tag to the ring, and leave the
/// registration takeable.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the macOS Metal present registers handlers")
)]
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

/// The most recent registration, once. The presenting thread takes it right
/// after its present returns; `None` when nothing registered since the last take
/// (a refused present, or a path with no handler).
pub fn take_registration() -> Option<Registration> {
    match TAKEABLE_SERIAL.swap(0, Ordering::Relaxed) {
        0 => None,
        serial => Some(Registration {
            serial,
            registered_ns: TAKEABLE_NS.load(Ordering::Relaxed),
        }),
    }
}

fn ring_slot(serial: u64) -> usize {
    usize::try_from(serial % TAG_RING as u64).unwrap_or(0)
}

/// Hand one handler's outcome to the ledger, naming the registration with the
/// next serial as its successor when one exists.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the macOS Metal present registers handlers")
)]
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

/// Classify one presented handler's reading. Both arguments are
/// `CACurrentMediaTime` seconds: `registered_s` read just before
/// `presentDrawable:`, `presented_s` the drawable's `presentedTime`.
#[must_use]
pub fn classify(registered_s: f64, presented_s: f64) -> GlassSample {
    if presented_s.is_nan() || presented_s <= 0.0 {
        return GlassSample::Skipped;
    }
    let secs = presented_s - registered_s;
    // The registration read and the display time come from one clock, so a
    // negative span is not expected; clamp rather than wrap if one ever lands.
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

    /// The registration statics are process-wide; the tests that drive them
    /// take turns.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn a_zero_presented_time_is_a_skip_not_a_duration() {
        assert_eq!(classify(12.5, 0.0), GlassSample::Skipped);
        assert_eq!(classify(12.5, f64::NAN), GlassSample::Skipped);
        assert_eq!(classify(12.5, -1.0), GlassSample::Skipped);
    }

    #[test]
    fn an_on_glass_frame_reports_registration_to_display() {
        let GlassSample::OnGlass { ns } = classify(100.0, 100.016) else {
            panic!("a nonzero presentedTime is on glass");
        };
        assert!(
            (15_999_000..=16_001_000).contains(&ns),
            "16 ms span read as {ns} ns"
        );
    }

    #[test]
    fn a_display_time_before_registration_clamps_to_zero() {
        assert_eq!(classify(100.0, 99.0), GlassSample::OnGlass { ns: 0 });
    }

    /// `ALL` is dense and in `index` order, and every label is distinct — the
    /// per-tag ledgers index by it and the wire prints it.
    #[test]
    fn tags_are_dense_ordered_and_distinctly_labelled() {
        for (i, tag) in PresentTag::ALL.iter().enumerate() {
            assert_eq!(tag.index(), i);
            assert_eq!(PresentTag::from_u8(*tag as u8), *tag);
        }
        let mut labels: Vec<&str> = PresentTag::ALL.iter().map(|t| t.as_str()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), PresentTag::COUNT);
        assert_eq!(PresentTag::from_u8(200), PresentTag::Spaced);
    }

    /// Arguments are `(burst, input_hot, occluded)`: occlusion wins, then the
    /// burst splits on the bypass, and a spaced present is `Spaced` either way.
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

    /// A skip is charged to the lane that replaced it, an occluded present to
    /// itself, and an unreplaced one to the compositor (`Spaced`).
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

    /// The tag is decided AT REGISTRATION on the registration clock: the same
    /// armed facts read `Spaced` when the registration lands a full period
    /// after the previous one and `HotBurst` when it lands inside it — and the
    /// registration is takeable exactly once, carrying its instant.
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

        // 8.33 ms later, bypass live: inside one period → hot burst.
        arm_present(ArmedPresent {
            input_hot: true,
            occluded: false,
            interval_ns: hz60_ns,
            prev_registered_ns: Some(reg.registered_ns),
        });
        let (_, tag) = registration_begin(100.008_33);
        assert_eq!(tag, PresentTag::HotBurst);

        // A full period later, bypass over: spaced, whatever arm time was.
        arm_present(ArmedPresent {
            input_hot: false,
            occluded: false,
            interval_ns: hz60_ns,
            prev_registered_ns: Some(reg.registered_ns),
        });
        let (_, tag) = registration_begin(100.016_67);
        assert_eq!(tag, PresentTag::Spaced);

        // Inside the period without the bypass: two lanes colliding.
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

    /// A report names the NEXT serial's tag as its successor, whether the handler
    /// fires before or after later registrations, and its own tag when nothing
    /// has registered after it.
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
        // The process sink may already be installed by another test; observe
        // through `delivered` and, when this sink won, through its captures.
        let ours = super::install_sink(sink);
        let before = delivered();
        // a's handler fires AFTER b and c registered: its successor is b, not c.
        deliver_report(a, tag_a, GlassSample::Skipped);
        // c has no successor yet: its own tag.
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
