// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn lid(lid: Lid) -> Reading<LidReading> {
    Reading::Read(LidReading {
        lid,
        changed_ms: None,
    })
}

fn displays(external: u16) -> Reading<Displays> {
    Reading::Read(Displays { external })
}

/// A4 §15 `a_desktop_draws_no_docked_lid_or_battery_glyph`, the model's half: a desktop
/// (no lid) with two external displays reads no docked state at all, while the laptop with
/// one, open or in clamshell, reads docked, and with none reads undocked. An unread lid or
/// display reader reads unread, never undocked.
/// NEGATIVE CONTROL: deciding docked by the displays alone (`Read(Some(d.external > 0))`
/// for any lid) fails the first assertion: `Read(Some(true))` on the Mac mini.
#[test]
fn a_desktop_reads_no_docked_state_and_a_laptop_at_a_desk_reads_docked() {
    let mini = Situation {
        lid: lid(Lid::None),
        displays: displays(2),
        ..Situation::default()
    };
    assert_eq!(mini.docked(), Reading::Read(None), "a desktop draws none");
    for l in [Lid::Open, Lid::Closed] {
        let at_desk = Situation {
            lid: lid(l),
            displays: displays(1),
            ..Situation::default()
        };
        assert_eq!(at_desk.docked(), Reading::Read(Some(true)), "{l:?}");
        let away = Situation {
            lid: lid(l),
            displays: displays(0),
            ..Situation::default()
        };
        assert_eq!(away.docked(), Reading::Read(Some(false)), "{l:?}");
    }
    let unread = Situation {
        lid: lid(Lid::Open),
        ..Situation::default()
    };
    assert_eq!(unread.docked(), Reading::Unread);
    assert_eq!(Situation::default().docked(), Reading::Unread);
    let off_macos = Situation {
        lid: Reading::Unsupported,
        displays: Reading::Unsupported,
        ..Situation::default()
    };
    assert_eq!(off_macos.docked(), Reading::Unsupported);
}

/// Nothing unread reads as a value: each state names its own `sys` word, and the default
/// situation is unread throughout.
/// NEGATIVE CONTROL: a `why` that answers `None` for `Unread` fails the second assertion.
#[test]
fn an_unread_value_says_why_and_never_reads_as_a_value() {
    assert_eq!(Reading::Read(1u8).why(), None);
    assert_eq!(Reading::<u8>::Unread.why(), Some("not_sampled"));
    assert_eq!(Reading::<u8>::Unsupported.why(), Some("unsupported"));
    for word in ["not_sampled", "unsupported"] {
        assert!(crate::sys::is_unavailable_word(word), "{word}");
    }
    let s = Situation::default();
    assert_eq!(s.machine.read(), None);
    assert_eq!(s.lid.read(), None);
    assert_eq!(s.power.read(), None);
    assert_eq!(s.displays.read(), None);
    assert_eq!(s.awake.read(), None);
}

/// Awake since the last sleep's end, else since boot.
#[test]
fn awake_since_is_the_last_wake_else_the_boot() {
    let mut a = Awake {
        boot_ms: 1_000,
        last_sleep: None,
    };
    assert_eq!(a.since_ms(), 1_000);
    a.last_sleep = Some(SleepSpan {
        from_ms: 5_000,
        to_ms: 9_000,
        cause: Some("Clamshell Sleep".into()),
        wake: Some("RTP.multi-touch".into()),
    });
    assert_eq!(a.since_ms(), 9_000);
}

fn stay(place: Option<crate::model::PlaceKind>, power: PowerSource, lid: Lid) -> Stay {
    Stay {
        place,
        power: Some(power),
        lid: Some(lid),
    }
}

/// A stay goes on through each reading that follows the one before with no gap, ends where the
/// next begins, and a reading across a gap starts a span of its own; a reading no reader
/// answered is in none.
/// CONTROL: a reading that joins nothing (`None`) with the same stay does not stretch the span
/// across the gap.
#[test]
fn a_span_holds_through_joined_readings_and_ends_at_a_change_or_a_gap() {
    use crate::model::PlaceKind;
    let desk = stay(Some(PlaceKind::Lan), PowerSource::Ac, Lid::Open);
    let cafe = stay(Some(PlaceKind::Hotspot), PowerSource::Battery, Lid::Open);
    let mut spans = Vec::new();
    Span::note(&mut spans, 1_000, desk, None, 8);
    Span::note(&mut spans, 2_000, desk, Some(1_000), 8);
    Span::note(&mut spans, 3_000, desk, Some(2_000), 8);
    assert_eq!(
        spans,
        [Span {
            start_ms: 1_000,
            end_ms: 3_000,
            stay: desk
        }]
    );
    // A change at a joined reading: the new stay starts where the last one ended.
    Span::note(&mut spans, 4_000, cafe, Some(3_000), 8);
    Span::note(&mut spans, 5_000, cafe, Some(4_000), 8);
    assert_eq!(
        spans[1..],
        [Span {
            start_ms: 3_000,
            end_ms: 5_000,
            stay: cafe
        }]
    );
    assert_eq!(spans[0].end_ms, 3_000);
    // An unread reading is in no stay, and the reading after it joins none that held through it.
    Span::note(&mut spans, 6_000, Stay::default(), Some(5_000), 8);
    Span::note(&mut spans, 7_000, cafe, Some(6_000), 8);
    Span::note(&mut spans, 8_000, cafe, Some(7_000), 8);
    assert_eq!(spans.len(), 3);
    assert_eq!((spans[1].end_ms, spans[2].start_ms), (5_000, 7_000));
    // CONTROL: the same stay across a gap is a span of its own.
    Span::note(&mut spans, 900_000, cafe, None, 8);
    assert_eq!(
        spans[2..],
        [
            Span {
                start_ms: 7_000,
                end_ms: 8_000,
                stay: cafe
            },
            Span {
                start_ms: 900_000,
                end_ms: 900_000,
                stay: cafe
            }
        ]
    );
    // A reading nothing joined covers no time: it gives way to the next span, so readings that
    // come apart (a hidden deck's, one every half minute) never spend the list.
    for at in (930_000..9_000_000).step_by(30_000) {
        Span::note(&mut spans, at, cafe, None, 8);
    }
    assert_eq!(spans.len(), 4, "{spans:?}");
    assert_eq!(
        spans[..3],
        [
            Span {
                start_ms: 1_000,
                end_ms: 3_000,
                stay: desk
            },
            Span {
                start_ms: 3_000,
                end_ms: 5_000,
                stay: cafe
            },
            Span {
                start_ms: 7_000,
                end_ms: 8_000,
                stay: cafe
            },
        ],
        "the stays that covered time are all kept"
    );
    assert_eq!((spans[3].start_ms, spans[3].end_ms), (8_970_000, 8_970_000));
}

/// A clock stepped back leaves no span ahead of the reading, the list keeps its newest `cap`,
/// and a span that ends before it starts is not put.
#[test]
fn spans_stay_in_order_under_a_step_back_and_within_their_cap() {
    use crate::model::PlaceKind;
    let desk = stay(Some(PlaceKind::Lan), PowerSource::Ac, Lid::Open);
    let dark = stay(Some(PlaceKind::Offline), PowerSource::Ac, Lid::Closed);
    let mut spans = Vec::new();
    Span::note(&mut spans, 10_000, desk, None, 3);
    Span::note(&mut spans, 11_000, desk, Some(10_000), 3);
    Span::note(&mut spans, 50_000, dark, None, 3);
    Span::note(&mut spans, 10_500, dark, None, 3);
    assert_eq!(
        spans,
        [
            Span {
                start_ms: 10_000,
                end_ms: 10_500,
                stay: desk
            },
            Span {
                start_ms: 10_500,
                end_ms: 10_500,
                stay: dark
            }
        ]
    );
    // Three stays that each cover time, one more than the cap keeps.
    for at in [20_000, 30_000, 40_000] {
        Span::note(&mut spans, at, desk, None, 3);
        Span::note(&mut spans, at + 1_000, desk, Some(at), 3);
    }
    assert_eq!(
        spans
            .iter()
            .map(|s| (s.start_ms, s.end_ms))
            .collect::<Vec<_>>(),
        [(20_000, 21_000), (30_000, 31_000), (40_000, 41_000)]
    );
    let before = spans.clone();
    Span::put(
        &mut spans,
        Span {
            start_ms: 60_000,
            end_ms: 59_999,
            stay: desk,
        },
        3,
    );
    assert_eq!(spans, before);
}

/// The stay a reading is in: netplace's kind for the place, none while it is still checking;
/// the power source and the lid where their readers answered.
#[test]
fn a_stay_reads_the_places_kind_the_power_source_and_the_lid() {
    use crate::model::PlaceKind;
    let situation = Situation {
        lid: lid(Lid::Closed),
        power: Reading::Read(Power {
            source: PowerSource::Battery,
            battery: Some(Battery {
                pct: 53,
                charging: false,
            }),
            adapter_w: None,
            changed_ms: None,
        }),
        ..Situation::default()
    };
    assert_eq!(
        Stay::of(Some(PlaceKind::Hotspot), &situation),
        stay(Some(PlaceKind::Hotspot), PowerSource::Battery, Lid::Closed)
    );
    assert_eq!(
        Stay::of(Some(PlaceKind::Checking), &situation).place,
        None,
        "checking is no kind"
    );
    assert!(Stay::of(None, &Situation::default()).is_unread());
    assert!(!Stay::of(Some(PlaceKind::Offline), &Situation::default()).is_unread());
}

const T0: i64 = 1_792_000_800_000;

/// Reading `t` (seconds) taken at `place`, on `power`, the lid as `l` says.
fn taken(
    t: i64,
    place: Option<crate::model::PlaceKind>,
    power: PowerSource,
    l: Lid,
) -> crate::model::Sample {
    let ms = u64::try_from(t).expect("small") * 1000;
    crate::model::Sample {
        at_ns: ms * 1_000_000,
        mono_ms: ms,
        at_unix_ms: T0 + t * 1000,
        cadence_ms: 1000,
        place,
        situation: Situation {
            lid: lid(l),
            power: Reading::Read(Power {
                source: power,
                battery: None,
                adapter_w: None,
                changed_ms: None,
            }),
            ..Situation::default()
        },
        ..crate::model::Sample::default()
    }
}

/// A4's situation spans, kept (N3): each stay holds netplace's kind for the place the host read
/// (`PlaceView::kind`: lan, hotspot, cellular, vpn, offline) with the power source and the lid,
/// from the reading that last read the stay before to the last that read it; the checkpoint's
/// base keeps them, a tail line brings a base up to them, and a relaunch restores them with none
/// stretched into the new run.
/// CONTROL: the same readings with no place read keep one span with no kind at all, never
/// `lan`; while netplace is still checking the span holds no kind; and readings no reader
/// answered are in no span.
#[test]
fn a_situation_span_keeps_netplaces_kind() {
    use crate::model::{PlaceKind, SavedStrip, StripLine, Vitals};
    use PowerSource::{Ac, Battery};
    // (first reading, last reading, place, power, lid), seconds.
    let stays = [
        (1, 5, Some(PlaceKind::Lan), Ac, Lid::Open),
        (6, 9, Some(PlaceKind::Hotspot), Battery, Lid::Open),
        (10, 12, Some(PlaceKind::Checking), Battery, Lid::Open),
        (13, 20, Some(PlaceKind::Cellular), Battery, Lid::Open),
        (21, 24, Some(PlaceKind::Vpn), Ac, Lid::Open),
        (25, 31, Some(PlaceKind::Offline), Ac, Lid::Closed),
        (32, 40, Some(PlaceKind::Offline), Ac, Lid::Open),
    ];
    let mut v = Vitals::new();
    let mut early = None;
    for (from, to, place, power, l) in stays {
        for t in from..=to {
            let _ = v.absorb(&taken(t, place, power, l));
            if t == 3 {
                early = Some(v.saved_strip(false));
            }
        }
    }
    let at = |t: i64| T0 + t * 1000;
    let span = |from: i64, to: i64, place, power, l| Span {
        start_ms: at(from),
        end_ms: at(to),
        stay: stay(place, power, l),
    };
    // Each stay starts at the last reading of the one before: the time between two readings
    // belongs to the stay it ended in.
    let want = [
        span(1, 5, Some(PlaceKind::Lan), Ac, Lid::Open),
        span(5, 9, Some(PlaceKind::Hotspot), Battery, Lid::Open),
        span(9, 12, None, Battery, Lid::Open),
        span(12, 20, Some(PlaceKind::Cellular), Battery, Lid::Open),
        span(20, 24, Some(PlaceKind::Vpn), Ac, Lid::Open),
        span(24, 31, Some(PlaceKind::Offline), Ac, Lid::Closed),
        span(31, 40, Some(PlaceKind::Offline), Ac, Lid::Open),
    ];
    assert_eq!(v.strip_spans(), want);
    assert_eq!(
        v.strip_spans()
            .iter()
            .map(|s| s.stay.place.map_or("", PlaceKind::word))
            .collect::<Vec<_>>(),
        [
            "lan", "hotspot", "", "cellular", "vpn", "offline", "offline"
        ],
        "netplace's kinds, and none while it was checking"
    );

    // The base keeps them.
    let kept = v.saved_strip(true);
    assert_eq!(kept.situation, want);
    // A base written at reading 3 holds the first stay as it then stood; the line that carries
    // the spans it does not hold brings it up to the model's.
    let mut base: SavedStrip = early.expect("saved at reading 3");
    assert_eq!(
        base.situation,
        [span(1, 3, Some(PlaceKind::Lan), Ac, Lid::Open)]
    );
    let moved: Vec<Span> = v
        .strip_spans()
        .iter()
        .filter(|s| !base.situation.contains(s))
        .copied()
        .collect();
    assert_eq!(
        moved.len(),
        want.len(),
        "the first stay moved, the rest are new"
    );
    assert!(base.replay(StripLine {
        k: base.through_k + 1,
        wall_ms: at(40),
        spans: moved,
        ..StripLine::default()
    }));
    assert_eq!(base.situation, want);

    // A relaunch a minute later restores them, and the new run's first reading in the same
    // stay starts a span of its own: nobody read the minute between.
    let mut relaunched = Vitals::new();
    relaunched.restore_strip_history(kept, at(100));
    assert_eq!(relaunched.strip_spans(), want);
    let _ = relaunched.absorb(&taken(101, Some(PlaceKind::Offline), Ac, Lid::Open));
    assert_eq!(relaunched.strip_spans().len(), want.len() + 1);
    assert_eq!(
        relaunched.strip_spans().last(),
        Some(&span(101, 101, Some(PlaceKind::Offline), Ac, Lid::Open))
    );
    assert_eq!(relaunched.strip_spans()[..want.len()], want);
    // Six hours on, none is kept.
    let mut later = Vitals::new();
    later.restore_strip_history(
        v.saved_strip(true),
        at(40) + crate::model::STRIP_HORIZON_MS + 1,
    );
    assert_eq!(later.strip_spans(), []);

    // CONTROL: no place read. One span, no kind.
    let mut placeless = Vitals::new();
    for t in 1..=9 {
        let _ = placeless.absorb(&taken(t, None, Ac, Lid::Open));
    }
    assert_eq!(placeless.strip_spans(), [span(1, 9, None, Ac, Lid::Open)]);
    // CONTROL: nothing read at all. No span.
    let mut blind = Vitals::new();
    for t in 1..=9 {
        let _ = blind.absorb(&crate::model::Sample {
            situation: Situation::default(),
            ..taken(t, None, Ac, Lid::Open)
        });
    }
    assert_eq!(blind.strip_spans(), []);
    assert_eq!(blind.saved_strip(true).situation, []);
}

/// A stay goes on through a reading that came late by less than a strip box (a loaded Mac's,
/// which reads no rate and still opens no gap): two hundred readings 4 s apart at a 1 s cadence
/// are one span, where the rule a rate reads would have made each a span of its own and spent
/// the list's 128.
/// CONTROL: readings half a minute apart are gaps, so none joins the one before and no time is
/// claimed between them; and a reading across a sleep (the wall clock a minute on, the
/// monotonic one a second) starts a span of its own.
#[test]
fn a_late_reading_stays_in_its_span_and_one_across_a_gap_does_not() {
    use crate::model::{PlaceKind, Vitals};
    let home = |t| taken(t, Some(PlaceKind::Lan), PowerSource::Ac, Lid::Open);
    let mut v = Vitals::new();
    for n in 0..200 {
        let _ = v.absorb(&home(1 + 4 * n));
    }
    assert_eq!(
        v.strip_spans()
            .iter()
            .map(|s| (s.start_ms - T0, s.end_ms - T0))
            .collect::<Vec<_>>(),
        [(1_000, 797_000)]
    );
    // CONTROL: half a minute apart.
    let mut apart = Vitals::new();
    for n in 0..200 {
        let _ = apart.absorb(&home(1 + 30 * n));
    }
    assert_eq!(
        apart
            .strip_spans()
            .iter()
            .map(|s| (s.start_ms - T0, s.end_ms - T0))
            .collect::<Vec<_>>(),
        [(5_971_000, 5_971_000)]
    );
    // CONTROL: a sleep between two readings a second apart on the monotonic clock.
    let woke = crate::model::Sample {
        at_unix_ms: T0 + 859_000,
        ..home(798)
    };
    let _ = v.absorb(&woke);
    let _ = v.absorb(&crate::model::Sample {
        at_unix_ms: T0 + 860_000,
        ..home(799)
    });
    assert_eq!(
        v.strip_spans()
            .iter()
            .map(|s| (s.start_ms - T0, s.end_ms - T0))
            .collect::<Vec<_>>(),
        [(1_000, 797_000), (859_000, 860_000)]
    );
}
