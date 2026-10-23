// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure state and deadline model for caffeine wake assertions.
//!
//! A timed mode's deadline is on the boot clock ([`Clocks::since_boot`]), which counts time
//! asleep and which no change of the date or time moves, so a span lasts what it was set for
//! whatever the wall clock does; only what the person reads is on the wall clock
//! ([`Clocks::wall_at`]).

/// Seconds in an hour.
pub const HOUR: u64 = 60 * 60;

/// The longest span `aterm caffeine on <span>` takes.
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

    /// The word `aterm caffeine on` takes for it.
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
    /// Whole seconds since this Mac booted, time asleep included (`mach_continuous_time`): no
    /// change of the date or time moves it, and every deadline is on it.
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
    /// On for `span` seconds from `now`, or [`Mode::Forever`] for `None`.
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

    /// What a click at `now` turns the mug to: off when it is on, forever when it is off.
    #[inline]
    #[must_use]
    pub const fn toggled(self, now: Clocks) -> Self {
        if self.is_on(now) {
            Self::Off
        } else {
            Self::Forever
        }
    }

    /// This mode as it stands at `now`: off past its deadline, and with no more than its span
    /// left (a deadline further off, which no choice sets, such as a hand-edited file's, comes
    /// in to its span).
    #[inline]
    #[must_use]
    pub const fn settled(self, now: Clocks) -> Self {
        let now = now.since_boot;
        match self {
            Self::Until { deadline, .. } if deadline <= now => Self::Off,
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
mod tests {
    use super::*;

    const NOW: Clocks = Clocks {
        since_boot: 300_000,
        wall: 1_790_000_000,
    };
    const DAY: u64 = 24 * HOUR;
    const TIMED: [Choice; 3] = [Choice::Hours1, Choice::Hours8, Choice::Hours24];

    #[test]
    fn a_click_turns_it_on_forever_and_the_next_one_off() {
        let on = Mode::Off.toggled(NOW);
        assert_eq!(on, Mode::Forever);
        assert_eq!(on.toggled(NOW + 5), Mode::Off);
        let timed = Mode::chosen(Choice::Hours8, NOW);
        assert_eq!(timed.toggled(NOW + 1), Mode::Off);
        assert_eq!(timed.toggled(NOW + 8 * HOUR), Mode::Forever);
    }

    #[test]
    fn a_choice_sets_a_deadline_its_span_away() {
        for choice in Choice::ALL {
            let mode = Mode::chosen(choice, NOW);
            assert!(mode.is_on(NOW), "{choice:?}");
            assert_eq!(mode.choice(NOW), Some(choice));
            match choice.span() {
                None => assert_eq!(mode, Mode::Forever),
                Some(span) => {
                    assert_eq!(
                        mode,
                        Mode::Until {
                            deadline: NOW.since_boot + span,
                            span
                        }
                    );
                    assert_eq!(mode.remaining(NOW), Some(span));
                    assert!(mode.is_on(NOW + (span - 1)));
                    assert!(!mode.is_on(NOW + span));
                    assert_eq!(mode.settled(NOW + span), Mode::Off);
                    assert_eq!(mode.choice(NOW + span), None);
                }
            }
        }
    }

    #[test]
    fn off_and_forever_have_no_time_left() {
        assert_eq!(Mode::Off.remaining(NOW), None);
        assert_eq!(Mode::Forever.remaining(NOW), None);
        let end = Clocks {
            since_boot: u64::MAX,
            wall: u64::MAX,
        };
        assert_eq!(Mode::Forever.settled(end), Mode::Forever);
        assert_eq!(Mode::activate(Some(0), NOW), Mode::Off);
        assert_eq!(
            Mode::activate(Some(HOUR), end),
            Mode::Until {
                deadline: u64::MAX,
                span: HOUR
            }
        );
    }

    /// The wall clock set back a day ten minutes into a span (by the person, or by the Mac
    /// correcting a clock that ran fast) leaves the span the time it had: it keeps the Mac awake
    /// no longer than it was set for, and its end on the wall clock moves back with the date.
    #[test]
    fn a_wall_clock_set_back_keeps_no_span_longer_than_it_was_set_for() {
        for choice in TIMED {
            let span = choice.span().expect("timed");
            let mode = Mode::chosen(choice, NOW);
            let back = Clocks {
                since_boot: NOW.since_boot + 600,
                wall: NOW.wall - DAY + 600,
            };
            assert_eq!(mode.remaining(back), Some(span - 600), "{choice:?}");
            assert_eq!(mode.choice(back), Some(choice), "{choice:?}");
            assert_eq!(
                back.wall_at(NOW.since_boot + span),
                NOW.wall - DAY + span,
                "{choice:?}"
            );
            assert!(mode.is_on(back + (span - 601)), "{choice:?}");
            assert!(
                !mode.is_on(back + (span - 600)),
                "{choice:?}: ended on time"
            );
            assert_eq!(mode.settled(back + (span - 600)), Mode::Off, "{choice:?}");
        }
    }

    /// The wall clock set forward a day ten minutes into a span ends nothing early.
    #[test]
    fn a_wall_clock_set_forward_ends_no_span_early() {
        for choice in TIMED {
            let span = choice.span().expect("timed");
            let mode = Mode::chosen(choice, NOW);
            let ahead = Clocks {
                since_boot: NOW.since_boot + 600,
                wall: NOW.wall + DAY + 600,
            };
            assert!(mode.is_on(ahead), "{choice:?}");
            assert_eq!(mode.remaining(ahead), Some(span - 600), "{choice:?}");
            assert_eq!(mode.choice(ahead), Some(choice), "{choice:?}");
            assert_eq!(
                mode.toggled(ahead),
                Mode::Off,
                "{choice:?}: a click turns it off"
            );
        }
    }

    /// Time asleep counts, as the boot clock counts it: a span a sleep outlasts has ended at the
    /// wake, and one that outlasts the sleep has that much less left.
    #[test]
    fn time_asleep_counts_toward_a_span() {
        let woke = NOW + 2 * HOUR;
        assert_eq!(Mode::chosen(Choice::Hours1, NOW).settled(woke), Mode::Off);
        assert!(!Mode::chosen(Choice::Hours1, NOW).is_on(woke));
        assert_eq!(
            Mode::chosen(Choice::Hours8, NOW).remaining(woke),
            Some(6 * HOUR)
        );
    }

    /// However its deadline came to be, a timed mode never has more than its span left, and
    /// settles to that; settled, it ends a span later.
    #[test]
    fn no_timed_mode_has_more_than_its_span_left() {
        let far = Mode::Until {
            deadline: NOW.since_boot + DAY,
            span: HOUR,
        };
        assert!(far.is_on(NOW));
        assert_eq!(far.remaining(NOW), Some(HOUR));
        assert_eq!(far.choice(NOW), Some(Choice::Hours1));
        let settled = far.settled(NOW);
        assert_eq!(settled, Mode::chosen(Choice::Hours1, NOW));
        assert_eq!(settled.settled(NOW + HOUR), Mode::Off);
        let near = Mode::chosen(Choice::Hours8, NOW);
        assert_eq!(
            near.settled(NOW + 60),
            near,
            "a deadline its span away is kept"
        );
    }

    #[test]
    fn a_span_no_menu_item_names_checks_none() {
        let mode = Mode::activate(Some(90 * 60), NOW);
        assert!(mode.is_on(NOW));
        assert_eq!(mode.choice(NOW), None);
    }

    #[test]
    fn span_words_parse() {
        assert_eq!(parse_span("forever"), Ok(None));
        for choice in Choice::ALL {
            assert_eq!(parse_span(choice.word()), Ok(choice.span()), "{choice:?}");
        }
        assert_eq!(parse_span("90m"), Ok(Some(90 * 60)));
        assert_eq!(parse_span("2h"), Ok(Some(2 * HOUR)));
        assert_eq!(parse_span("8760h"), Ok(Some(MAX_SPAN)));
        for bad in [
            "",
            "h",
            "m",
            "1",
            "1s",
            "-1h",
            "+1h",
            "1.5h",
            "0h",
            "0m",
            "8761h",
            "1 h",
            "forever!",
            "99999999999999999999h",
        ] {
            assert!(parse_span(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn time_left_reads_as_hours_and_minutes() {
        assert_eq!(describe_left(0), "less than a minute");
        assert_eq!(describe_left(59), "less than a minute");
        assert_eq!(describe_left(60), "1 min");
        assert_eq!(describe_left(52 * 60 + 30), "52 min");
        assert_eq!(describe_left(HOUR), "1 h");
        assert_eq!(describe_left(7 * HOUR + 12 * 60 + 59), "7 h 12 min");
        assert_eq!(describe_left(24 * HOUR), "24 h");
    }

    #[test]
    fn every_choice_has_its_own_label_word_and_span() {
        for (i, a) in Choice::ALL.iter().enumerate() {
            assert_eq!(Choice::from_span(a.span()), Some(*a));
            for b in &Choice::ALL[i + 1..] {
                assert_ne!(a.label(), b.label());
                assert_ne!(a.word(), b.word());
                assert_ne!(a.span(), b.span());
            }
        }
    }
}
