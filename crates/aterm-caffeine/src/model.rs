// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether, and until when, this Mac is kept awake: the mug's whole model, pure.
//!
//! Times are whole seconds since the Unix epoch (wall clock), passed in by the caller, so a
//! deadline does not move when the Mac sleeps and every rule here is a plain function.

/// Seconds in an hour.
pub const HOUR: u64 = 60 * 60;

/// The longest span `aterm caffeine on <span>` takes: a year. Forever is its own mode.
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
    /// Activate Forever: until a click turns it off.
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

/// Whether, and until when, this Mac is kept awake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Not keeping the Mac awake.
    Off,
    /// Keeping it awake until turned off.
    Forever,
    /// Keeping it awake until `deadline` (seconds since the Unix epoch), set as a span of
    /// `span` seconds (what the menu checks).
    Until {
        /// When it turns off.
        deadline: u64,
        /// The span it was set for.
        span: u64,
    },
}

impl Mode {
    /// On for `span` seconds from `now`, or [`Mode::Forever`] for `None`. A zero span is off.
    #[inline]
    #[must_use]
    pub const fn activate(span: Option<u64>, now: u64) -> Self {
        match span {
            None => Self::Forever,
            Some(0) => Self::Off,
            Some(span) => Self::Until {
                deadline: now.saturating_add(span),
                span,
            },
        }
    }

    /// What a menu choice made at `now` turns the mug to.
    #[inline]
    #[must_use]
    pub const fn chosen(choice: Choice, now: u64) -> Self {
        Self::activate(choice.span(), now)
    }

    /// What a click at `now` turns the mug to: off when it is on, forever when it is off.
    #[inline]
    #[must_use]
    pub const fn toggled(self, now: u64) -> Self {
        if self.is_on(now) {
            Self::Off
        } else {
            Self::Forever
        }
    }

    /// This mode as it stands at `now`: a deadline that has passed is off.
    #[inline]
    #[must_use]
    pub const fn settled(self, now: u64) -> Self {
        match self {
            Self::Until { deadline, .. } if deadline <= now => Self::Off,
            other => other,
        }
    }

    /// Whether the Mac is kept awake at `now`.
    #[inline]
    #[must_use]
    pub const fn is_on(self, now: u64) -> bool {
        match self {
            Self::Off => false,
            Self::Forever => true,
            Self::Until { deadline, .. } => deadline > now,
        }
    }

    /// Seconds left at `now` of a timed mode; `None` when off or forever.
    #[inline]
    #[must_use]
    pub const fn remaining(self, now: u64) -> Option<u64> {
        match self.settled(now) {
            Self::Until { deadline, .. } => Some(deadline - now),
            Self::Off | Self::Forever => None,
        }
    }

    /// The menu choice this mode was set by at `now`: none when off or when the span is not a
    /// menu one (`aterm caffeine on 90m`).
    #[inline]
    #[must_use]
    pub const fn choice(self, now: u64) -> Option<Choice> {
        match self.settled(now) {
            Self::Off => None,
            Self::Forever => Some(Choice::Forever),
            Self::Until { span, .. } => Choice::from_span(Some(span)),
        }
    }
}

/// Parse a span word: `forever`, a menu word (`1h`, `8h`, `24h`), or `<n>h` / `<n>m` up to
/// [`MAX_SPAN`]. `Ok(None)` is forever.
///
/// # Errors
///
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

/// `secs` left, as a person reads it: `7 h 12 min`, `52 min`, `1 h`, `less than a minute`.
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

    const NOW: u64 = 1_790_000_000;

    #[test]
    fn a_click_turns_it_on_forever_and_the_next_one_off() {
        let on = Mode::Off.toggled(NOW);
        assert_eq!(on, Mode::Forever);
        assert_eq!(on.toggled(NOW + 5), Mode::Off);
        let timed = Mode::chosen(Choice::Hours8, NOW);
        assert_eq!(timed.toggled(NOW + 1), Mode::Off);
        // A deadline that passed is off, so the click turns it on again.
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
                            deadline: NOW + span,
                            span
                        }
                    );
                    assert_eq!(mode.remaining(NOW), Some(span));
                    assert!(mode.is_on(NOW + span - 1));
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
        assert_eq!(Mode::Forever.settled(u64::MAX), Mode::Forever);
        assert_eq!(Mode::activate(Some(0), NOW), Mode::Off);
        // A deadline past the end of time saturates instead of wrapping.
        assert_eq!(
            Mode::activate(Some(HOUR), u64::MAX),
            Mode::Until {
                deadline: u64::MAX,
                span: HOUR
            }
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
