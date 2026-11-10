// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The machine's situation and type as the vitals thread reads them into each
//! [`aterm_vitals::model::Sample`] (stats deck A4 §8 and §9): the readers `aterm_sysprobe`'s
//! [`situation`](aterm_sysprobe::situation) module keeps current on notifications, the machine's
//! type read once, and the sleep and wake listener whose wake has the watch read the last sleep.
//! A reading takes what they hold; nothing here wakes on a timer. The network place is not read
//! here: the snapshot carries netplace's own place (`crate::net_place::sys_view`).

use std::sync::Arc;

use aterm_sysprobe::situation as probe;
use aterm_vitals::situation::{
    Awake, Battery, Displays, Lid, LidReading, MachineType, Power, PowerSource, Reading, Situation,
    SleepSpan,
};

/// The situation's readers, held by one vitals [`crate::vitals_host::Reader`].
pub(crate) struct SituationReader {
    /// The computer's type, read once as the reader starts.
    machine: Reading<Arc<MachineType>>,
    /// The power sources, read when their tokens say they changed; `None` in a unit test's
    /// reader, which reads no power manager.
    power: Option<probe::PowerWatch>,
    /// The lid, the external displays and the last sleep; `None` in a unit test's reader.
    watch: Option<probe::Watch>,
    /// The sleep and wake listener whose wake has the watch read the lid and the last sleep
    /// again; it hears until dropped.
    _woke: Option<aterm_uds::power::Listening>,
}

impl SituationReader {
    /// The live readers: the type read now, the power tokens registered, the watch started and
    /// told each wake.
    pub(crate) fn host() -> Self {
        let watch = probe::Watch::start();
        let handle = watch.handle();
        let woke = aterm_uds::power::listen(Box::new(move |power| {
            if power == aterm_uds::power::Power::Woke {
                handle.woke();
            }
        }));
        if woke.is_none() && probe::SLEEP_READER {
            aterm_log::info!(
                "system vitals: no wake announcement; the last sleep reads as it was when the \
                 situation watch started, and the lid as its messages say"
            );
        }
        Self {
            machine: aterm_sysprobe::machine_type::read().map_or(Reading::Unsupported, |t| {
                Reading::Read(Arc::new(machine_type(t)))
            }),
            power: Some(probe::PowerWatch::new()),
            watch: Some(watch),
            _woke: woke,
        }
    }

    /// A unit test's reader: it reads nothing, and every value reads unread.
    pub(crate) fn inert() -> Self {
        Self {
            machine: Reading::Unread,
            power: None,
            watch: None,
            _woke: None,
        }
    }

    /// The situation at a reading at `now_ms` (unix): what the watch has heard, and the power
    /// sources (read again only when a token was posted).
    pub(crate) fn read(&mut self, now_ms: i64) -> Situation {
        let power = self.power.as_mut().and_then(|p| p.read(now_ms));
        let heard = self
            .watch
            .as_ref()
            .map(probe::Watch::heard)
            .unwrap_or_default();
        situation(self.machine.clone(), power, heard)
    }
}

/// `value` as a reading where its reader exists (`supported`): read, else unread; else
/// unsupported.
fn reading<T>(supported: bool, value: Option<T>) -> Reading<T> {
    match (supported, value) {
        (false, _) => Reading::Unsupported,
        (true, Some(v)) => Reading::Read(v),
        (true, None) => Reading::Unread,
    }
}

/// The situation the readers' values make: each unread where its reader has not answered and
/// unsupported where this OS has none ([`probe::LID_READER`] and its siblings).
pub(crate) fn situation(
    machine: Reading<Arc<MachineType>>,
    power: Option<probe::PowerState>,
    heard: probe::Heard,
) -> Situation {
    Situation {
        machine,
        lid: reading(
            probe::LID_READER,
            heard.lid.map(|l| LidReading {
                lid: match l.lid {
                    probe::Lid::Open => Lid::Open,
                    probe::Lid::Closed => Lid::Closed,
                    probe::Lid::None => Lid::None,
                },
                changed_ms: l.changed_ms,
            }),
        ),
        power: reading(probe::POWER_READER, power.map(power_of)),
        displays: reading(
            probe::DISPLAY_READER,
            heard
                .external_displays
                .map(|external| Displays { external }),
        ),
        awake: reading(probe::SLEEP_READER, heard.awake.map(awake_of)),
    }
}

fn power_of(p: probe::PowerState) -> Power {
    Power {
        source: match p.reading.source {
            probe::PowerSource::Ac => PowerSource::Ac,
            probe::PowerSource::Battery => PowerSource::Battery,
            probe::PowerSource::Ups => PowerSource::Ups,
        },
        battery: p.reading.battery.map(|b| Battery {
            pct: b.pct,
            charging: b.charging,
        }),
        adapter_w: p.reading.adapter_w,
        changed_ms: p.changed_ms,
    }
}

fn awake_of(a: probe::Awake) -> Awake {
    Awake {
        boot_ms: a.boot_ms,
        last_sleep: a.last_sleep.map(|s| SleepSpan {
            from_ms: s.from_ms,
            to_ms: s.to_ms,
            cause: s.cause,
            wake: s.wake,
        }),
    }
}

fn machine_type(t: aterm_sysprobe::machine_type::MachineType) -> MachineType {
    MachineType {
        name: t.name,
        family: t.family,
        model: t.model,
        model_id: t.model_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that answered reads its value; one that has not reads unread; one this OS lacks
    /// reads unsupported, never a value or a zero. The displays' unread state leaves docked
    /// unread, not undocked.
    /// NEGATIVE CONTROL: `reading` mapping `(true, None)` to `Unsupported` fails the second
    /// assertion.
    #[test]
    fn an_unanswered_reader_reads_unread_and_a_missing_one_unsupported() {
        assert_eq!(reading(true, Some(3u8)), Reading::Read(3));
        assert_eq!(reading::<u8>(true, None), Reading::Unread);
        assert_eq!(reading(false, Some(3u8)), Reading::Unsupported);
        let s = situation(
            Reading::Unread,
            None,
            probe::Heard {
                lid: Some(probe::LidState {
                    lid: probe::Lid::Open,
                    changed_ms: None,
                }),
                external_displays: None,
                awake: None,
            },
        );
        assert_eq!(
            s.lid.read().map(|l| l.lid),
            probe::LID_READER.then_some(Lid::Open)
        );
        assert_ne!(
            s.docked(),
            Reading::Read(Some(false)),
            "never undocked unread"
        );
        assert_eq!(
            s.power.why(),
            Some(if probe::POWER_READER {
                "not_sampled"
            } else {
                "unsupported"
            })
        );
    }

    /// A unit test's reader reads nothing: no value of its situation is a reading.
    #[test]
    fn a_unit_tests_reader_reads_nothing() {
        let s = SituationReader::inert().read(0);
        assert_eq!(s.machine.read(), None);
        assert_eq!(s.lid.read(), None);
        assert_eq!(s.power.read(), None);
        assert_eq!(s.displays.read(), None);
        assert_eq!(s.awake.read(), None);
    }
}
