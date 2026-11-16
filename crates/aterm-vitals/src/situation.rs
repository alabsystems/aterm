// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The machine's situation and its type, as one reading holds them (stats deck A4 §8 and §9,
//! `docs/DESIGN-stats-deck-instruments-2026-11-06.md`): the lid, the power source and the
//! battery, the external displays (whether a laptop sits docked), the last sleep and its cause,
//! and the computer's type. The host reads each where it changes (a notification, a token
//! checked each reading, once at its start) and hands it in on the [`crate::model::Sample`];
//! the reading's [`crate::model::Snapshot::situation`] carries it out to the deck, `sys` and
//! the event detector.
//!
//! The network place is not here: it is [`crate::model::Snapshot::place`], netplace's own place,
//! which the situation points at rather than copies (A4 §12.5). Low Power Mode is not here either:
//! it is [`crate::model::Thermal::low_power`]. The strip's history of the situation is its
//! [`Span`]s: each stay with the place's kind, the power source and the lid.
//!
//! Every value says when it is not a reading ([`Reading::Unread`], [`Reading::Unsupported`]);
//! none stands in a zero. What a machine lacks (a desktop's lid, battery or built-in panel) is
//! a reading of its own ([`Lid::None`], [`Power::battery`] `None`, [`Situation::docked`]'s
//! `Read(None)`), so a deck draws no glyph for it and `sys` says so.

use std::sync::Arc;

/// A situational reading, or why there is none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Reading<T> {
    /// Read.
    Read(T),
    /// This OS has a reader for it, which has not answered: not yet, or it was refused.
    #[default]
    Unread,
    /// This OS has no reader for it.
    Unsupported,
}

impl<T> Reading<T> {
    /// The value, where read.
    #[must_use]
    pub const fn read(&self) -> Option<&T> {
        match self {
            Self::Read(v) => Some(v),
            Self::Unread | Self::Unsupported => None,
        }
    }

    /// The `sys` unavailable word for a value that is not read ([`crate::sys::UNAVAILABLE_WORDS`]):
    /// `not_sampled` or `unsupported`; `None` for a reading.
    #[must_use]
    pub const fn why(&self) -> Option<&'static str> {
        match self {
            Self::Read(_) => None,
            Self::Unread => Some("not_sampled"),
            Self::Unsupported => Some("unsupported"),
        }
    }

    /// The reading of `f` of the value; the same why where there is none.
    #[must_use]
    pub fn map<U>(&self, f: impl FnOnce(&T) -> U) -> Reading<U> {
        match self {
            Self::Read(v) => Reading::Read(f(v)),
            Self::Unread => Reading::Unread,
            Self::Unsupported => Reading::Unsupported,
        }
    }
}

/// The lid, as the power manager says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lid {
    /// Open.
    Open,
    /// Closed (asleep, or in clamshell at a desk).
    Closed,
    /// This machine has no lid: a desktop. The deck draws no lid glyph for it.
    None,
}

impl Lid {
    /// The word `sys` and the lens say: `open`, `closed`, `none`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::None => "none",
        }
    }
}

/// The lid's reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LidReading {
    /// Open, closed or none.
    pub lid: Lid,
    /// When the closed bit last changed while a listener heard it (unix ms); `None` until it
    /// changed since the host started (the state read at its start has no moment).
    pub changed_ms: Option<i64>,
}

/// What powers the machine now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerSource {
    /// A wall adapter.
    Ac,
    /// The internal battery.
    Battery,
    /// An uninterruptible power supply's battery.
    Ups,
}

impl PowerSource {
    /// The word `sys` and `await sys` say: `ac`, `battery`, `ups`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Ac => "ac",
            Self::Battery => "battery",
            Self::Ups => "ups",
        }
    }
}

/// The internal battery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    /// Its charge, 0–100 %.
    pub pct: u8,
    /// Whether it is charging now (on AC and below full).
    pub charging: bool,
}

/// The power source's reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Power {
    /// What powers the machine.
    pub source: PowerSource,
    /// The internal battery; `None` on a machine with none (a desktop: the deck draws a plug).
    pub battery: Option<Battery>,
    /// The wall adapter's rating in watts, where the OS reports one (on AC); `None` on battery
    /// or where it is not reported.
    pub adapter_w: Option<u32>,
    /// When the source last switched while the host read it (unix ms, the reading that saw the
    /// switch); `None` until it switched since the host started.
    pub changed_ms: Option<i64>,
}

/// The displays attached beside the machine's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Displays {
    /// External displays attached now.
    pub external: u16,
}

/// One sleep, as the kernel and the power manager record it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SleepSpan {
    /// When it went to sleep (unix ms, `kern.sleeptime`).
    pub from_ms: i64,
    /// When it woke (unix ms, `kern.waketime`).
    pub to_ms: i64,
    /// Why it slept, in the power manager's words (`Clamshell Sleep`, `Idle Sleep`,
    /// `Software Sleep`); `None` where it gave none.
    pub cause: Option<String>,
    /// What woke it, the wake reason's last word (`RTP.multi-touch`); `None` where none was given.
    pub wake: Option<String>,
}

/// The machine's waking life since it booted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Awake {
    /// When the kernel booted (unix ms).
    pub boot_ms: i64,
    /// The last sleep since boot; `None` when it has not slept.
    pub last_sleep: Option<SleepSpan>,
}

impl Awake {
    /// Since when the machine has been awake (unix ms): the end of its last sleep, else its boot.
    #[must_use]
    pub fn since_ms(&self) -> i64 {
        self.last_sleep.as_ref().map_or(self.boot_ms, |s| s.to_ms)
    }
}

/// The computer's type (A4 §9), read once at the host's start. Each name passed the fallback
/// ladder's filter: none is a machine-type code or a firmware placeholder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MachineType {
    /// The name the Machine row draws (`MacBook Air`, `Lenovo ThinkPad X1 Carbon Gen 12`);
    /// `None` where nothing passed the ladder, so the OS's machine noun stands
    /// ([`crate::words::Platform::machine`]).
    pub name: Option<String>,
    /// The shorter family form a name too wide for its slot takes (`Dell Latitude`); `None`
    /// where the name is already its family (every Mac) or no family passed.
    pub family: Option<String>,
    /// The long form (`MacBook Air (15-inch, M5)`); `None` where the firmware gives no longer
    /// form than the name (every machine off macOS).
    pub model: Option<String>,
    /// The firmware's identifier for the model (`Mac17,4`, `21K9CTO1WW`), which may be a code:
    /// it is printed for agents and the lens, never drawn as the name.
    pub model_id: Option<String>,
}

/// The machine's situation at one reading. Its network place is [`crate::model::Snapshot::place`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Situation {
    /// The computer's type, read once.
    pub machine: Reading<Arc<MachineType>>,
    /// The lid.
    pub lid: Reading<LidReading>,
    /// The power source and the battery.
    pub power: Reading<Power>,
    /// The external displays.
    pub displays: Reading<Displays>,
    /// The machine's sleeps and wakes.
    pub awake: Reading<Awake>,
}

impl Situation {
    /// Whether this is a laptop at a desk: an external display attached to a machine with a
    /// built-in panel, which a lid reader answering says it has (A4 §8). `Read(None)` on a
    /// machine with no lid (a desktop, whose every display is external, so the glyph would
    /// always be lit and say nothing): the deck draws no docked glyph there.
    #[must_use]
    pub fn docked(&self) -> Reading<Option<bool>> {
        match (&self.lid, &self.displays) {
            (Reading::Read(l), _) if l.lid == Lid::None => Reading::Read(None),
            (Reading::Read(_), Reading::Read(d)) => Reading::Read(Some(d.external > 0)),
            (Reading::Unsupported, _) | (_, Reading::Unsupported) => Reading::Unsupported,
            _ => Reading::Unread,
        }
    }
}

/// What a situation span keeps of one reading (A4 §8's lane): the network place's kind, the
/// power source and the lid. A value no reader gave is `None`, and draws blank.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stay {
    /// netplace's kind for the place ([`crate::model::PlaceView::kind`]): `lan`, `hotspot`,
    /// `cellular`, `vpn` or `offline`. `None` while netplace has none yet (it is checking) or
    /// the host read no place.
    pub place: Option<crate::model::PlaceKind>,
    /// What powers the machine.
    pub power: Option<PowerSource>,
    /// The lid ([`Lid::None`] on a machine with none).
    pub lid: Option<Lid>,
}

impl Stay {
    /// The stay a reading of `place` and `situation` is in.
    #[must_use]
    pub fn of(place: Option<crate::model::PlaceKind>, situation: &Situation) -> Self {
        Self {
            place: place.filter(|k| *k != crate::model::PlaceKind::Checking),
            power: situation.power.read().map(|p| p.source),
            lid: situation.lid.read().map(|l| l.lid),
        }
    }

    /// Whether no reader gave any of it: such a reading is in no stay.
    #[must_use]
    pub const fn is_unread(&self) -> bool {
        self.place.is_none() && self.power.is_none() && self.lid.is_none()
    }
}

/// One stay of the machine's situation, `[start_ms, end_ms)` in unix ms: the readings over which
/// its place, power source and lid read the same, one after another with no gap. The strip keeps
/// six hours of them, as it keeps its gaps and events, and draws the Machine row's lane from
/// them (A4 §8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// The first reading in it; where it follows another stay with no gap, that stay's end.
    pub start_ms: i64,
    /// The newest reading in it.
    pub end_ms: i64,
    /// What read the same throughout.
    pub stay: Stay,
}

impl Span {
    /// Puts `span` into `spans`, oldest first, keeping at most `cap`: every span that starts at
    /// or after it goes (it replaces the one it continues, and a clock stepped back leaves none
    /// ahead of it), and the one before it ends no later than it starts. A span of one reading
    /// that nothing joined covers no time, so it gives way to the span put after it: readings
    /// that come apart, as a hidden deck's do, never spend the list. A span that ends before it
    /// starts is not put.
    pub fn put(spans: &mut Vec<Self>, span: Self, cap: usize) {
        if span.end_ms < span.start_ms {
            return;
        }
        spans.retain(|s| s.start_ms < span.start_ms);
        if spans
            .last()
            .is_some_and(|last| last.start_ms == last.end_ms)
        {
            spans.pop();
        }
        if let Some(last) = spans.last_mut() {
            last.end_ms = last.end_ms.min(span.start_ms);
        }
        spans.push(span);
        let extra = spans.len().saturating_sub(cap);
        spans.drain(..extra);
    }

    /// Takes a reading at `at_ms` in `stay` into `spans` ([`Self::put`]). `joins` is the wall
    /// clock of the reading before it where this one follows it with no gap (no sleep, pause or
    /// clock step between them): the stay that held through that reading goes on, or ends there
    /// and the new one starts. Any other reading starts a span of its own, which stays only
    /// once a reading joins it, and an unread stay none.
    pub fn note(spans: &mut Vec<Self>, at_ms: i64, stay: Stay, joins: Option<i64>, cap: usize) {
        if stay.is_unread() {
            return;
        }
        let held = spans
            .last()
            .zip(joins)
            .filter(|(last, prev)| last.end_ms == *prev && at_ms >= *prev)
            .map(|(last, prev)| {
                if last.stay == stay {
                    last.start_ms
                } else {
                    prev
                }
            });
        Self::put(
            spans,
            Self {
                start_ms: held.unwrap_or(at_ms),
                end_ms: at_ms,
                stay,
            },
            cap,
        );
    }
}

#[cfg(test)]
#[path = "situation_tests.rs"]
mod tests;
