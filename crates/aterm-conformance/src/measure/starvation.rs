// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The instrument's account of its own run: was the take evidence?
//!
//! A starved driver or a recorder that missed its cadence makes a take that
//! says nothing about the glass. The account decides that one thing, in one
//! direction: starvation may take a green away (unproved), never rescue a red.
//! It reads this process's own scheduling (a fixed-interval timer's lateness,
//! [`Sampler`]) and the recorder's cadence (the scan's sampling hole against
//! its nominal interval); neither sees inside the subject, so a clean reading
//! disowns only driver starvation.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The lateness floor: about twice the worst lateness a saturated 18-core
/// machine withheld from a 50 ms sampler (13 ms), and more than one capture
/// interval, so a trip means the driver lost at least a frame's worth.
pub const DEFAULT_LATE_THRESHOLD_US: u64 = 25_000;
/// A take that used 9/10 of its budget was racing its own deadline.
pub const BUDGET_PRESSURE_NUM: u64 = 9;
pub const BUDGET_PRESSURE_DEN: u64 = 10;

/// The tier this process family runs in (`qos_class_self`). A `launchctl
/// submit` job runs at utility, which the subject it launches inherits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Qos {
    UserInteractive,
    UserInitiated,
    Default,
    Utility,
    Background,
    Unspecified,
}

impl Qos {
    /// The calling thread's class, or `None` off macOS or for an unknown value.
    #[must_use]
    pub fn current() -> Option<Self> {
        Self::from_raw(raw_qos()?)
    }

    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Self> {
        Some(match raw {
            0x21 => Self::UserInteractive,
            0x19 => Self::UserInitiated,
            0x15 => Self::Default,
            0x11 => Self::Utility,
            0x09 => Self::Background,
            0x00 => Self::Unspecified,
            _ => return None,
        })
    }

    /// A tier where every take is starved by construction: the remedy is the
    /// tier, not another take.
    #[must_use]
    pub const fn clamped(self) -> bool {
        matches!(self, Self::Utility | Self::Background)
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::UserInteractive => "user-interactive",
            Self::UserInitiated => "user-initiated",
            Self::Default => "default",
            Self::Utility => "utility",
            Self::Background => "background",
            Self::Unspecified => "unspecified",
        }
    }
}

#[cfg(target_vendor = "apple")]
unsafe extern "C" {
    // `qos_class_t qos_class_self(void)`: reads the calling thread's class.
    safe fn qos_class_self() -> u32;
}

#[cfg(target_vendor = "apple")]
fn raw_qos() -> Option<u32> {
    Some(qos_class_self())
}

#[cfg(not(target_vendor = "apple"))]
fn raw_qos() -> Option<u32> {
    None
}

/// What the starvation reading of one take is, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Starvation {
    /// No take was produced: a driver-side reading cannot tell a starved
    /// subject from a dark one.
    NoTake,
    /// The sampler took no ticks.
    NoSamples,
    /// The driver was descheduled past the floor.
    Descheduled {
        late_max_us: u64,
        threshold_us: u64,
        qos: Option<Qos>,
    },
    /// The recorder's capture cadence blew out: the frames scored are not the
    /// frames asked for.
    CadenceBlown { hole_us: u64, nominal_us: u64 },
    /// The take used most of its budget.
    BudgetPressure { wall_us: u64, budget_us: u64 },
    /// The driver kept its cadence. Only driver starvation is disowned.
    KeptCadence {
        late_max_us: u64,
        threshold_us: u64,
        ticks: u64,
    },
}

/// The one-word label of a [`Starvation`] reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Starved {
    Yes,
    No,
    /// The question was not asked: never a clean bill.
    Unknown,
}

/// Whether a take may be used as evidence about paint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence {
    Sound,
    Unproved,
}

/// The shape's own expectation, before the instrument's account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeVerdict {
    Pass,
    Fail,
}

/// A take's final disposition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    Pass,
    Fail,
    /// The take ran and is not evidence: neither a pass nor a paint failure.
    Unproved,
}

impl Starvation {
    #[must_use]
    pub const fn label(&self) -> Starved {
        match self {
            Self::NoTake | Self::NoSamples => Starved::Unknown,
            Self::Descheduled { .. } | Self::CadenceBlown { .. } | Self::BudgetPressure { .. } => {
                Starved::Yes
            }
            Self::KeptCadence { .. } => Starved::No,
        }
    }

    /// Only a reading that the driver kept its cadence is sound.
    #[must_use]
    pub fn evidence(&self) -> Evidence {
        if self.label() == Starved::No {
            Evidence::Sound
        } else {
            Evidence::Unproved
        }
    }
}

impl std::fmt::Display for Starvation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTake => f.write_str(
                "no take was produced; a driver-side reading cannot see the subject's render or \
                 recorder",
            ),
            Self::NoSamples => f.write_str("no scheduling samples were taken"),
            Self::Descheduled {
                late_max_us,
                threshold_us,
                qos,
            } => {
                write!(
                    f,
                    "the driver was descheduled for {late_max_us}us (>= {threshold_us}us): not \
                     evidence about paint"
                )?;
                match qos {
                    Some(q) if q.clamped() => write!(
                        f,
                        "; this process family runs at QoS {} (a submitted job's tier, which the \
                         subject inherits): run from an interactive shell or a LaunchAgent with \
                         ProcessType=Interactive",
                        q.name()
                    ),
                    _ => f.write_str("; re-take on an unloaded machine"),
                }
            }
            Self::CadenceBlown {
                hole_us,
                nominal_us,
            } => write!(
                f,
                "the capture cadence blew out: hole {hole_us}us is more than twice the nominal \
                 {nominal_us}us: not evidence about paint"
            ),
            Self::BudgetPressure { wall_us, budget_us } => write!(
                f,
                "the take used {wall_us}us of its {budget_us}us budget: not evidence about paint"
            ),
            Self::KeptCadence {
                late_max_us,
                threshold_us,
                ticks,
            } => write!(
                f,
                "the driver kept its cadence (worst {late_max_us}us of a {threshold_us}us floor \
                 over {ticks} ticks); the subject's own render is not visible to this reading"
            ),
        }
    }
}

/// The inputs of one classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedInputs {
    pub ticks: u64,
    pub late_max_us: u64,
    pub sampling_hole_us: u64,
    pub nominal_dt_us: u64,
    pub wall_us: u64,
    pub budget_us: u64,
    pub late_threshold_us: u64,
    pub produced_take: bool,
    pub qos: Option<Qos>,
}

/// Read one take's starvation. Unknown outranks everything: with no samples
/// the question was not asked.
#[must_use]
pub const fn classify(i: &SchedInputs) -> Starvation {
    if !i.produced_take {
        return Starvation::NoTake;
    }
    if i.ticks == 0 {
        return Starvation::NoSamples;
    }
    let late_max_us = i.late_max_us;
    if late_max_us >= i.late_threshold_us {
        return Starvation::Descheduled {
            late_max_us,
            threshold_us: i.late_threshold_us,
            qos: i.qos,
        };
    }
    if i.nominal_dt_us > 0 && i.sampling_hole_us > 2 * i.nominal_dt_us {
        return Starvation::CadenceBlown {
            hole_us: i.sampling_hole_us,
            nominal_us: i.nominal_dt_us,
        };
    }
    if i.budget_us > 0 && i.wall_us * BUDGET_PRESSURE_DEN >= i.budget_us * BUDGET_PRESSURE_NUM {
        return Starvation::BudgetPressure {
            wall_us: i.wall_us,
            budget_us: i.budget_us,
        };
    }
    Starvation::KeptCadence {
        late_max_us,
        threshold_us: i.late_threshold_us,
        ticks: i.ticks,
    }
}

/// Compose the shape's verdict with the take's standing: a pass the
/// instrument cannot stand behind is unproved; a fail stays a fail. A function
/// of one take, so repeating a starved take accumulates nothing.
#[must_use]
pub const fn paint_verdict(shape: ShapeVerdict, evidence: Evidence) -> Disposition {
    match (shape, evidence) {
        (ShapeVerdict::Pass, Evidence::Sound) => Disposition::Pass,
        (ShapeVerdict::Pass, Evidence::Unproved) => Disposition::Unproved,
        (ShapeVerdict::Fail, _) => Disposition::Fail,
    }
}

/// What the [`Sampler`] saw over its life.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchedReading {
    pub ticks: u64,
    pub tick_us: u64,
    pub late_max_us: u64,
    pub late_total_us: u64,
    /// When the worst tick fell, from the sampler's start.
    pub late_max_at_us: u64,
    /// The ticks booked while the capture window was open.
    pub win_ticks: u64,
    pub win_late_max_us: u64,
    pub win_late_total_us: u64,
    pub win_first_at_us: Option<u64>,
    pub win_last_at_us: Option<u64>,
    pub sampled_us: u64,
    pub qos: Option<Qos>,
}

impl SchedReading {
    /// Book one tick whose deadline fell `deadline` after the origin and which
    /// woke `woke` after it; `in_window` when the capture window was open at
    /// either edge of the tick, so a deschedule straddling the open or the
    /// close counts.
    pub fn book(&mut self, deadline: Duration, woke: Duration, in_window: bool) {
        let late = micros(woke.saturating_sub(deadline));
        let at = micros(deadline);
        self.ticks += 1;
        self.late_total_us += late;
        if late > self.late_max_us {
            self.late_max_us = late;
            self.late_max_at_us = at;
        }
        if in_window {
            self.win_ticks += 1;
            self.win_first_at_us.get_or_insert(at);
            self.win_last_at_us = Some(at);
            self.win_late_total_us += late;
            self.win_late_max_us = self.win_late_max_us.max(late);
        }
    }

    /// The reading that decides the standing: the capture window's when it was
    /// sampled at all (a deschedule outside it cannot change a frame), the
    /// whole life's otherwise.
    #[must_use]
    pub const fn deciding(&self) -> (u64, u64) {
        if self.win_ticks > 0 {
            (self.win_ticks, self.win_late_max_us)
        } else {
            (self.ticks, self.late_max_us)
        }
    }
}

/// A fixed-interval timer on its own thread, measuring how late each tick
/// wakes: the time the scheduler withheld from this process. Deadlines run
/// from a fixed origin, so a late tick is never absorbed into the next one.
pub struct Sampler {
    stop: Arc<AtomicBool>,
    window: Arc<AtomicBool>,
    thread: Option<JoinHandle<SchedReading>>,
}

impl Sampler {
    /// Start ticking every `tick`. At least one tick is booked however soon
    /// the sampler is finished.
    ///
    /// # Panics
    /// The sampler thread could not be started.
    #[must_use]
    pub fn start(tick: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let window = Arc::new(AtomicBool::new(false));
        let (s, w) = (Arc::clone(&stop), Arc::clone(&window));
        let (started, ready) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("paint-sampler".into())
            .spawn(move || sample(&s, &w, tick, &started))
            .expect("start the scheduling sampler");
        // The origin is set before this returns, so the sampled span covers
        // everything the caller does next.
        let _ = ready.recv();
        Self {
            stop,
            window,
            thread: Some(thread),
        }
    }

    /// Book the ticks from now on to the capture window as well.
    pub fn open_window(&self) {
        self.window.store(true, Ordering::SeqCst);
    }

    pub fn close_window(&self) {
        self.window.store(false, Ordering::SeqCst);
    }

    /// Stop and read. Idempotent: a second call reads nothing new.
    #[must_use]
    pub fn finish(&mut self) -> SchedReading {
        self.stop.store(true, Ordering::SeqCst);
        self.thread
            .take()
            .and_then(|t| t.join().ok())
            .unwrap_or_default()
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

fn sample(
    stop: &AtomicBool,
    window: &AtomicBool,
    tick: Duration,
    started: &std::sync::mpsc::Sender<()>,
) -> SchedReading {
    let tick = tick.max(Duration::from_millis(1));
    let origin = Instant::now();
    let mut r = SchedReading {
        tick_us: micros(tick),
        qos: Qos::current(),
        ..SchedReading::default()
    };
    let _ = started.send(());
    let mut n: u32 = 0;
    loop {
        let open_before = window.load(Ordering::SeqCst);
        n += 1;
        let deadline = tick * n;
        let now = origin.elapsed();
        if deadline > now {
            std::thread::sleep(deadline - now);
        }
        let woke = origin.elapsed();
        r.book(deadline, woke, open_before || window.load(Ordering::SeqCst));
        if stop.load(Ordering::SeqCst) {
            break;
        }
    }
    r.sampled_us = micros(origin.elapsed());
    r
}

#[cfg(test)]
#[path = "starvation_tests.rs"]
mod tests;
