// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure state and deadline model for acaffeine wake assertions, and what the mug holds for a
//! mode that is on given the lid and the system's sleep ([`hold`]).
//!
//! A timed mode's deadline is on the boot clock ([`Clocks::since_boot`]), which counts time
//! asleep and which no change of the date or time moves, so a span lasts what it was set for
//! whatever the wall clock does; only what the person reads is on the wall clock
//! ([`Clocks::wall_at`]).

/// Seconds in an hour.
pub const HOUR: u64 = 60 * 60;

/// The longest span `aterm acaffeine on <span>` takes.
pub const MAX_SPAN: u64 = 365 * 24 * HOUR;

/// One of the menu's Activate choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Activate for 1 Hour.
    Hours1,
    /// Activate for 8 Hours.
    Hours8,
    /// Activate for 24 Hours.
    Hours24,
    /// Activate Forever.
    Forever,
}

impl Choice {
    /// Every choice, in menu order.
    pub const ALL: [Self; 4] = [Self::Hours1, Self::Hours8, Self::Hours24, Self::Forever];

    /// The span this choice keeps the Mac awake for, `None` for [`Choice::Forever`].
    #[inline]
    #[must_use]
    pub const fn span(self) -> Option<u64> {
        match self {
            Self::Hours1 => Some(HOUR),
            Self::Hours8 => Some(8 * HOUR),
            Self::Hours24 => Some(24 * HOUR),
            Self::Forever => None,
        }
    }

    /// The menu item's title.
    #[inline]
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Hours1 => "Activate for 1 Hour",
            Self::Hours8 => "Activate for 8 Hours",
            Self::Hours24 => "Activate for 24 Hours",
            Self::Forever => "Activate Forever",
        }
    }

    /// The word `aterm acaffeine on` takes for it.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Hours1 => "1h",
            Self::Hours8 => "8h",
            Self::Hours24 => "24h",
            Self::Forever => "forever",
        }
    }

    /// The choice whose span is `span` exactly, if any.
    #[inline]
    #[must_use]
    pub const fn from_span(span: Option<u64>) -> Option<Self> {
        match span {
            Some(HOUR) => Some(Self::Hours1),
            Some(s) if s == 8 * HOUR => Some(Self::Hours8),
            Some(s) if s == 24 * HOUR => Some(Self::Hours24),
            None => Some(Self::Forever),
            Some(_) => None,
        }
    }
}

/// One reading of the two clocks a mode is kept and shown against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clocks {
    /// Whole seconds since this Mac booted, time asleep included (`mach_continuous_time`); every
    /// deadline is on it.
    pub since_boot: u64,
    /// Whole seconds since the Unix epoch, as the wall clock reads.
    pub wall: u64,
}

impl Clocks {
    /// What the wall clock reads at `deadline` on the boot clock, if the date is not changed
    /// before it: the wall clock now plus the time to it (now, for a deadline gone).
    #[inline]
    #[must_use]
    pub const fn wall_at(self, deadline: u64) -> u64 {
        self.wall
            .saturating_add(deadline.saturating_sub(self.since_boot))
    }
}

/// These clocks `secs` later, with no change of the date in between.
#[cfg(test)]
impl core::ops::Add<u64> for Clocks {
    type Output = Self;

    fn add(self, secs: u64) -> Self {
        Self {
            since_boot: self.since_boot + secs,
            wall: self.wall + secs,
        }
    }
}

/// Whether, and until when, this Mac is kept awake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Not keeping the Mac awake.
    Off,
    /// Keeping it awake until turned off.
    Forever,
    /// Keeping it awake until `deadline`.
    Until {
        /// When it turns off, on the boot clock ([`Clocks::since_boot`]).
        deadline: u64,
        /// The span it was set for.
        span: u64,
    },
}

impl Mode {
    /// On for `span` seconds from `now` (off for zero), or [`Mode::Forever`] for `None`.
    #[inline]
    #[must_use]
    pub const fn activate(span: Option<u64>, now: Clocks) -> Self {
        match span {
            None => Self::Forever,
            Some(0) => Self::Off,
            Some(span) => Self::Until {
                deadline: now.since_boot.saturating_add(span),
                span,
            },
        }
    }

    /// What a menu choice made at `now` turns the mug to.
    #[inline]
    #[must_use]
    pub const fn chosen(choice: Choice, now: Clocks) -> Self {
        Self::activate(choice.span(), now)
    }

    /// What a click at `now` turns the mug to: forever when it is off, else off. A span past its
    /// deadline that the mug's tick has not yet ended still shows the mug lit, so a click on it
    /// turns it off, as it does any lit mug, and as the span's end does.
    #[inline]
    #[must_use]
    pub const fn toggled(self, now: Clocks) -> Self {
        match self {
            Self::Until { .. } => Self::Off,
            other if other.is_on(now) => Self::Off,
            _ => Self::Forever,
        }
    }

    /// This mode at `now`: off past its deadline or with a span of nothing (a hand-edited `span=0`,
    /// which ends as it starts), and with no more than its span left (a deadline further off, such
    /// as a hand-edited file's, comes in to its span).
    #[inline]
    #[must_use]
    pub const fn settled(self, now: Clocks) -> Self {
        let now = now.since_boot;
        match self {
            Self::Until { deadline, span } if deadline <= now || span == 0 => Self::Off,
            Self::Until { deadline, span } if deadline - now > span => Self::Until {
                deadline: now + span,
                span,
            },
            other => other,
        }
    }

    /// Whether the Mac is kept awake at `now`.
    #[inline]
    #[must_use]
    pub const fn is_on(self, now: Clocks) -> bool {
        match self {
            Self::Off => false,
            Self::Forever => true,
            Self::Until { deadline, .. } => deadline > now.since_boot,
        }
    }

    /// Seconds left at `now` of a timed mode, at most its span.
    #[inline]
    #[must_use]
    pub const fn remaining(self, now: Clocks) -> Option<u64> {
        match self.settled(now) {
            Self::Until { deadline, .. } => Some(deadline - now.since_boot),
            Self::Off | Self::Forever => None,
        }
    }

    /// The menu choice this mode was set by at `now`.
    #[inline]
    #[must_use]
    pub const fn choice(self, now: Clocks) -> Option<Choice> {
        match self.settled(now) {
            Self::Off => None,
            Self::Forever => Some(Choice::Forever),
            Self::Until { span, .. } => Choice::from_span(Some(span)),
        }
    }
}

/// What the machine is doing, as the mug reads it before it decides what to hold ([`hold`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Machine {
    /// The lid is closed (the power manager's `AppleClamshellState`); a Mac with no lid reads
    /// open.
    pub lid_closed: bool,
    /// The Mac is in a dark wake: awake without graphics, as macOS wakes one with its lid closed
    /// for moments at a time.
    pub dark_wake: bool,
    /// The system announced it will sleep, and no wake has come since.
    pub going_to_sleep: bool,
}

/// Why a mode that is on holds nothing ([`Hold::Paused`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pause {
    /// The lid is closed, and the person did not ask to keep the Mac awake with it closed.
    LidClosed,
    /// The Mac is going to sleep, or is in a dark wake: macOS chose to sleep it (the lid's
    /// close on battery, Apple menu > Sleep, a thermal emergency), which no hold argues with,
    /// Keep Awake with Lid Closed included.
    Asleep,
}

/// What the mug holds ([`hold`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// Nothing: the mode is off.
    Off,
    /// Nothing, the mode staying on: it holds again once the pause ends (the lid opens, the Mac
    /// wakes fully), unless its span ended meanwhile.
    Paused(Pause),
    /// The activity: no idle sleep of the system or the display (`PreventUserIdleSystemSleep`,
    /// `PreventUserIdleDisplaySleep`).
    Idle,
    /// The activity and a `PreventSystemSleep` assertion, which keeps the Mac running with its
    /// lid closed while it is on AC power (macOS does not honour it on battery).
    LidClosedToo,
}

impl Hold {
    /// Whether the keep-awake activity is held.
    #[inline]
    #[must_use]
    pub const fn activity(self) -> bool {
        matches!(self, Self::Idle | Self::LidClosedToo)
    }

    /// Whether the `PreventSystemSleep` assertion is held.
    #[inline]
    #[must_use]
    pub const fn system(self) -> bool {
        matches!(self, Self::LidClosedToo)
    }
}

/// What the mug holds for a mode that is `on`, with Keep Awake with Lid Closed `lid_awake`, on
/// `machine`. A going-to-sleep or a dark wake holds nothing whatever was chosen, so macOS's own
/// sleep (the lid's close on battery, where `PreventSystemSleep` is not honoured, or a thermal
/// emergency) is never argued with: a Mac the opt-in keeps running lid-closed on AC power never
/// slept, so it is in a full wake, and a dark wake means macOS already slept it, which a hold
/// taken again would only stretch into a DarkWake loop. Otherwise the person's opt-in holds both
/// assertions, lid open or closed: the system assertion must already be held when the lid
/// closes, since macOS sleeps at the close. Without it, a closed lid pauses the hold, and an
/// open lid in a full wake holds the activity alone.
#[inline]
#[must_use]
pub const fn hold(on: bool, lid_awake: bool, machine: Machine) -> Hold {
    if !on {
        Hold::Off
    } else if machine.going_to_sleep || machine.dark_wake {
        Hold::Paused(Pause::Asleep)
    } else if lid_awake {
        Hold::LidClosedToo
    } else if machine.lid_closed {
        Hold::Paused(Pause::LidClosed)
    } else {
        Hold::Idle
    }
}

/// How long after a wake a paused hold is looked at again every second ([`settling`]).
///
/// A wake is announced before it has settled: an Apple-silicon Mac whose lid opens wakes dark
/// (the CPU without graphics) and gains graphics a fraction of a second later, and every signal
/// the mug hears (the system's power-on, `NSWorkspaceDidWakeNotification`, the displays'
/// reconfiguration) comes in that dark part, so the look each takes reads a dark wake and pauses.
/// Nothing announces the graphics' arrival to a process outside the kernel, so the mug looks
/// again until the pause ends or this long has passed; a wake that stays dark this long is a
/// dark wake, which the next tick reads again.
pub const SETTLE_SECS: u64 = 30;

/// Whether `hold`, decided at `now` on the boot clock, is a pause that may be a wake not yet
/// settled ([`SETTLE_SECS`]) after the wake at `woke_at`, so the mug looks again in a second
/// rather than at its next tick. A hold that is not paused has nothing to settle, and no wake
/// (none heard, or a will-sleep since) leaves none.
#[inline]
#[must_use]
pub const fn settling(hold: Hold, woke_at: Option<u64>, now: u64) -> bool {
    match (hold, woke_at) {
        (Hold::Paused(_), Some(at)) => now >= at && now - at < SETTLE_SECS,
        _ => false,
    }
}

/// Parse a span word: `forever` (`Ok(None)`), or `<n>h` / `<n>m` up to [`MAX_SPAN`].
///
/// # Errors
/// A word that is none of those, a zero span, or one longer than a year.
pub fn parse_span(word: &str) -> Result<Option<u64>, String> {
    if word == "forever" {
        return Ok(None);
    }
    let bad = || format!("{word:?} is not a span: 1h, 8h, 24h, forever, <n>h or <n>m");
    let (digits, unit) = match word.as_bytes().last() {
        Some(b'h') => (&word[..word.len() - 1], HOUR),
        Some(b'm') => (&word[..word.len() - 1], 60),
        _ => return Err(bad()),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: u64 = digits.parse().map_err(|_| bad())?;
    match n.checked_mul(unit) {
        Some(0) => Err(format!("{word:?} is no time at all")),
        Some(span) if span <= MAX_SPAN => Ok(Some(span)),
        _ => Err(format!("{word:?} is longer than a year; use forever")),
    }
}

/// `secs` left, as a person reads it.
#[must_use]
pub fn describe_left(secs: u64) -> String {
    let minutes = secs / 60;
    let (h, m) = (minutes / 60, minutes % 60);
    match (h, m) {
        (0, 0) => "less than a minute".to_string(),
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
